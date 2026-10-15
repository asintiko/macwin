//! Capture through an active CGEventTap on a dedicated thread.
//!
//! Fail-safes that work even if the Engine, the network or the peer is
//! broken (each releases the grab locally first, then reports a fault):
//! - the emergency chord (Ctrl+Alt+Shift+Esc), checked on physical keys
//!   before anything is swallowed;
//! - the grab lease, checked on every event and every 100 ms;
//! - the OS disabling the tap (timeout / user input);
//! - a panic in the callback.

use std::ffi::c_void;
use std::panic::{self, AssertUnwindSafe};
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use macwin_core::os::INJECT_TAG;
use macwin_core::os::macos::mask_from_flags;
use macwin_core::safety::EmergencyDetector;
use macwin_core::types::{CaptureFault, CapturedEvent, GrabCommand};
use macwin_platform::{CaptureControl, CaptureSink, InputCapture, PlatformError};
use objc2_core_foundation::{CFMachPort, CFRunLoop, kCFRunLoopDefaultMode};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType,
};
use tracing::{info, warn};

use crate::convert::{convert, event_mask};
use crate::cursor;

/// Lease granted by a Grab until the first renewal arrives.
const INITIAL_LEASE: Duration = Duration::from_secs(2);
/// Run-loop slice between stop and lease checks.
const SLICE_SECONDS: f64 = 0.1;

#[derive(Default)]
struct State {
    grabbed: bool,
    lease: Option<Instant>,
    cursor_hidden: bool,
    detector: EmergencyDetector,
}

struct Shared {
    sink: Arc<dyn CaptureSink>,
    state: Mutex<State>,
    stop: AtomicBool,
    /// The tap's `CFMachPort`, valid while the capture thread runs.
    tap: AtomicPtr<c_void>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Ungrabs locally (cursor back, visible). Returns whether a grab was active.
    fn release_locally(&self) -> bool {
        let mut st = self.lock();
        let was = st.grabbed;
        st.grabbed = false;
        st.lease = None;
        cursor::associate(true);
        if st.cursor_hidden {
            cursor::show();
            st.cursor_hidden = false;
        }
        was
    }

    fn apply(&self, cmd: GrabCommand) {
        let mut st = self.lock();
        match cmd {
            GrabCommand::Grab { exit } => {
                st.grabbed = true;
                st.lease = Some(Instant::now() + INITIAL_LEASE);
                cursor::warp(exit);
                cursor::associate(false);
                if !st.cursor_hidden {
                    cursor::hide();
                    st.cursor_hidden = true;
                }
            }
            GrabCommand::Release { warp_to } => {
                st.grabbed = false;
                st.lease = None;
                cursor::warp(warp_to);
                cursor::associate(true);
                if st.cursor_hidden {
                    cursor::show();
                    st.cursor_hidden = false;
                }
            }
            GrabCommand::Rearm => {
                st.grabbed = false;
                st.lease = None;
                cursor::associate(true);
                if st.cursor_hidden {
                    cursor::show();
                    st.cursor_hidden = false;
                }
            }
        }
    }

    fn notify_fault(&self, fault: CaptureFault) {
        warn!(?fault, "capture fail-safe released the grab");
        if panic::catch_unwind(AssertUnwindSafe(|| self.sink.on_fault(fault))).is_err() {
            warn!("the capture sink panicked while handling a fault");
        }
    }

    /// Releases the grab if its lease ran out (called every slice).
    fn check_lease(&self) {
        let expired = {
            let st = self.lock();
            st.grabbed && st.lease.is_some_and(|l| Instant::now() > l)
        };
        if expired && self.release_locally() {
            self.notify_fault(CaptureFault::LeaseExpired);
        }
    }

    fn reenable_tap(&self) {
        let tap = self.tap.load(Ordering::Acquire);
        if !tap.is_null() {
            // SAFETY: the pointer is the live CFMachPort owned by the capture
            // thread, which is the only thread running this callback.
            CGEvent::tap_enable(unsafe { &*(tap as *const CFMachPort) }, true);
        }
    }

    /// Returns whether the event reaches the local OS.
    fn handle(&self, t: CGEventType, e: &CGEvent) -> bool {
        if t == CGEventType::TapDisabledByTimeout || t == CGEventType::TapDisabledByUserInput {
            self.reenable_tap();
            if self.release_locally() {
                self.notify_fault(CaptureFault::TapDisabled);
            }
            return true;
        }
        if CGEvent::integer_value_field(Some(e), CGEventField::EventSourceUserData)
            == INJECT_TAG as i64
        {
            return true; // our own injection
        }
        let mut events = Vec::with_capacity(2);
        convert(t, e, &mut events);
        if events.is_empty() {
            return true;
        }
        let snapshot = mask_from_flags(CGEvent::flags(Some(e)).0);
        let fault = {
            let mut st = self.lock();
            let mut fault = None;
            for ev in &events {
                if let CapturedEvent::Key { hid, down, repeat } = *ev
                    && st.detector.on_key(hid, down, repeat, Some(snapshot))
                {
                    fault = Some(CaptureFault::EmergencyChord);
                }
            }
            if fault.is_none() && st.grabbed && st.lease.is_some_and(|l| Instant::now() > l) {
                fault = Some(CaptureFault::LeaseExpired);
            }
            fault
        };
        if let Some(f) = fault {
            self.release_locally();
            self.notify_fault(f);
            return true;
        }
        let mut deliver = true;
        for ev in events {
            deliver &= self.sink.on_event(ev);
        }
        deliver
    }
}

unsafe extern "C-unwind" fn tap_callback(
    _proxy: CGEventTapProxy,
    t: CGEventType,
    event: NonNull<CGEvent>,
    user_info: *mut c_void,
) -> *mut CGEvent {
    // SAFETY: `user_info` is the `Shared` the capture thread keeps alive for
    // as long as the tap exists.
    let shared = unsafe { &*(user_info as *const Shared) };
    // SAFETY: CG guarantees the event is valid for the duration of the callback.
    let e = unsafe { event.as_ref() };
    let deliver = match panic::catch_unwind(AssertUnwindSafe(|| shared.handle(t, e))) {
        Ok(deliver) => deliver,
        Err(_) => {
            shared.release_locally();
            shared.notify_fault(CaptureFault::CallbackPanic);
            true
        }
    };
    if deliver {
        event.as_ptr()
    } else {
        ptr::null_mut()
    }
}

fn run_tap(shared: Arc<Shared>, ready: mpsc::Sender<Result<(), PlatformError>>) {
    let user_info = Arc::as_ptr(&shared) as *mut c_void;
    // SAFETY: `tap_callback` follows the CGEventTapCallBack contract and
    // `user_info` points to `shared`, which this function keeps alive until
    // after the tap is invalidated.
    let tap = unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::SessionEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            event_mask(),
            Some(tap_callback),
            user_info,
        )
    };
    let Some(tap) = tap else {
        let _ = ready.send(Err(PlatformError::PermissionDenied(
            "cannot create the event tap: grant Accessibility (and Input Monitoring) to this app in \
             System Settings > Privacy & Security"
                .into(),
        )));
        return;
    };
    let Some(source) = CFMachPort::new_run_loop_source(None, Some(&tap), 0) else {
        let _ = ready.send(Err(PlatformError::Backend(
            "cannot create the tap run-loop source".into(),
        )));
        return;
    };
    let Some(run_loop) = CFRunLoop::current() else {
        let _ = ready.send(Err(PlatformError::Backend(
            "no run loop on the capture thread".into(),
        )));
        return;
    };
    // SAFETY: a CF constant valid for the process lifetime.
    let mode = unsafe { kCFRunLoopDefaultMode };
    shared.tap.store(
        (&*tap as *const CFMachPort).cast_mut().cast(),
        Ordering::Release,
    );
    run_loop.add_source(Some(&source), mode);
    CGEvent::tap_enable(&tap, true);
    let _ = ready.send(Ok(()));
    info!("event tap running");
    while !shared.stop.load(Ordering::Acquire) {
        CFRunLoop::run_in_mode(mode, SLICE_SECONDS, false);
        shared.check_lease();
    }
    CGEvent::tap_enable(&tap, false);
    run_loop.remove_source(Some(&source), mode);
    tap.invalidate();
    shared.tap.store(ptr::null_mut(), Ordering::Release);
    shared.release_locally();
    info!("event tap stopped");
}

/// CGEventTap capture backend.
#[derive(Default)]
pub struct MacCapture {
    thread: Option<JoinHandle<()>>,
    shared: Option<Arc<Shared>>,
}

impl MacCapture {
    pub fn new() -> MacCapture {
        MacCapture::default()
    }
}

struct MacControl {
    shared: Arc<Shared>,
}

impl CaptureControl for MacControl {
    fn command(&self, cmd: GrabCommand) {
        self.shared.apply(cmd);
    }

    fn renew_lease(&self, until: Instant) {
        self.shared.lock().lease = Some(until);
    }

    fn stop(&self) {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.release_locally();
    }
}

impl InputCapture for MacCapture {
    fn start(
        &mut self,
        sink: Arc<dyn CaptureSink>,
    ) -> Result<Arc<dyn CaptureControl>, PlatformError> {
        if self.thread.is_some() {
            return Err(PlatformError::Backend("capture already started".into()));
        }
        let shared = Arc::new(Shared {
            sink,
            state: Mutex::new(State::default()),
            stop: AtomicBool::new(false),
            tap: AtomicPtr::new(ptr::null_mut()),
        });
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread_shared = shared.clone();
        let handle = thread::Builder::new()
            .name("macwin-capture".into())
            .spawn(move || run_tap(thread_shared, ready_tx))
            .map_err(|e| PlatformError::Backend(format!("cannot start the capture thread: {e}")))?;
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => {
                self.thread = Some(handle);
                self.shared = Some(shared.clone());
                Ok(Arc::new(MacControl { shared }))
            }
            Ok(Err(e)) => {
                let _ = handle.join();
                Err(e)
            }
            Err(_) => {
                shared.stop.store(true, Ordering::Release);
                shared.release_locally();
                Err(PlatformError::Backend(
                    "the capture thread did not start".into(),
                ))
            }
        }
    }
}

impl Drop for MacCapture {
    fn drop(&mut self) {
        if let Some(shared) = &self.shared {
            shared.stop.store(true, Ordering::Release);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
