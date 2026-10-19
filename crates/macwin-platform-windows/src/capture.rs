//! Capture through `WH_KEYBOARD_LL` / `WH_MOUSE_LL` on a dedicated thread
//! with its own message loop.
//!
//! Grab: swallowed mouse moves leave the cursor parked at the exit point,
//! so each move's `pt - park` is the post-acceleration delta. A tiny
//! layered topmost window with a NULL cursor sits under the parked cursor to
//! hide it. While grabbed, mouse moves never reach the OS (even if the Engine
//! disagrees; the lease ends such a mismatch) and a cursor moved by anything
//! else (touch, pen, another app) is parked again every 100 ms. Fail-safes
//! (released locally first, then reported): the emergency chord, the grab
//! lease (checked per event and every 100 ms) and a panic in the hook. Hooks
//! must return fast: Windows silently removes slow low-level hooks.

use std::panic::{self, AssertUnwindSafe};
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use macwin_core::os::left_park;
use macwin_core::os::windows::{LowLevelMouse, classify_key, classify_mouse, scan_to_hid};
use macwin_core::safety::EmergencyDetector;
use macwin_core::types::{CaptureFault, CapturedEvent, GrabCommand, Point, PointF, WheelUnit};
use macwin_platform::{CaptureControl, CaptureSink, InputCapture, PlatformError};
use tracing::{info, warn};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos,
    GetMessageW, HC_ACTION, HHOOK, HWND_TOPMOST, KBDLLHOOKSTRUCT, KillTimer, LLKHF_EXTENDED,
    LLKHF_INJECTED, LLMHF_INJECTED, LWA_ALPHA, MSG, MSLLHOOKSTRUCT, PostThreadMessageW,
    RegisterClassExW, SW_HIDE, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetCursor, SetCursorPos,
    SetLayeredWindowAttributes, SetTimer, SetWindowPos, SetWindowsHookExW, ShowWindow,
    TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_APP, WM_QUIT,
    WM_SETCURSOR, WM_TIMER, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};

const INITIAL_LEASE: Duration = Duration::from_secs(2);
const WM_GRAB: u32 = WM_APP + 1;
const WM_RELEASE: u32 = WM_APP + 2;
const WM_REARM: u32 = WM_APP + 3;
/// Side of the cursor-hiding window.
const HIDER_SIZE: i32 = 16;

#[derive(Default)]
struct State {
    grabbed: bool,
    park: Point,
    last_pt: Option<Point>,
    lease: Option<Instant>,
    /// The cursor was found off `park` during this grab (logged once).
    drift_seen: bool,
    detector: EmergencyDetector,
}

struct Shared {
    sink: Arc<dyn CaptureSink>,
    state: Mutex<State>,
    thread_id: AtomicU32,
}

/// The single active capture (low-level hook procedures have no context
/// pointer).
static ACTIVE: Mutex<Option<Arc<Shared>>> = Mutex::new(None);

fn active() -> Option<Arc<Shared>> {
    ACTIVE.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn post(&self, msg: u32, p: Point) {
        let tid = self.thread_id.load(Ordering::Acquire);
        // SAFETY: posting to our own capture thread's queue; the coordinates
        // travel as plain integers.
        unsafe {
            PostThreadMessageW(tid, msg, p.x as isize as WPARAM, p.y as isize as LPARAM);
        }
    }

    /// Ungrabs locally. Returns whether a grab was active.
    fn release_locally(&self) -> bool {
        let was = {
            let mut st = self.lock();
            let was = st.grabbed;
            st.grabbed = false;
            st.lease = None;
            st.last_pt = None;
            was
        };
        if was {
            self.post(WM_REARM, Point::default());
        }
        was
    }

    fn notify_fault(&self, fault: CaptureFault) {
        warn!(?fault, "capture fail-safe released the grab");
        if panic::catch_unwind(AssertUnwindSafe(|| self.sink.on_fault(fault))).is_err() {
            warn!("the capture sink panicked while handling a fault");
        }
    }

    fn lease_expired(st: &State) -> bool {
        st.grabbed && st.lease.is_some_and(|l| Instant::now() > l)
    }

    fn check_lease(&self) {
        let expired = Self::lease_expired(&self.lock());
        if expired && self.release_locally() {
            self.notify_fault(CaptureFault::LeaseExpired);
        }
    }

    /// The parking point when the cursor is grabbed but no longer on it.
    fn drifted_park(&self) -> Option<Point> {
        let mut st = self.lock();
        if !st.grabbed {
            return None;
        }
        let mut pt = POINT { x: 0, y: 0 };
        // SAFETY: GetCursorPos only writes the POINT we own.
        if unsafe { GetCursorPos(&mut pt) } == 0 {
            return None; // e.g. the secure desktop is active
        }
        if !left_park(Point::new(pt.x, pt.y).to_f(), st.park) {
            return None;
        }
        if !st.drift_seen {
            st.drift_seen = true;
            warn!("the cursor moved while input was grabbed: parking it again");
        }
        Some(st.park)
    }

    /// Runs the fail-safes, then asks the sink. Returns true to swallow.
    fn dispatch(&self, ev: CapturedEvent, key: Option<(macwin_core::types::Hid, bool)>) -> bool {
        let fault = {
            let mut st = self.lock();
            if let Some((hid, down)) = key
                && st.detector.on_key(hid, down, false, None)
            {
                Some(CaptureFault::EmergencyChord)
            } else if Self::lease_expired(&st) {
                Some(CaptureFault::LeaseExpired)
            } else {
                None
            }
        };
        if let Some(f) = fault {
            self.release_locally();
            self.notify_fault(f);
            return false;
        }
        !self.sink.on_event(ev)
    }

    fn on_keyboard(&self, msg: u32, kb: &KBDLLHOOKSTRUCT) -> bool {
        if kb.flags & LLKHF_INJECTED != 0 {
            return false; // synthetic input, including our own
        }
        let Some(down) = classify_key(msg) else {
            return false;
        };
        let Some(hid) = scan_to_hid(kb.scanCode, kb.flags & LLKHF_EXTENDED != 0) else {
            return false;
        };
        // Windows reports auto-repeat as repeated downs; the Engine marks them.
        self.dispatch(
            CapturedEvent::Key {
                hid,
                down,
                repeat: false,
            },
            Some((hid, down)),
        )
    }

    fn on_mouse(&self, msg: u32, ms: &MSLLHOOKSTRUCT) -> bool {
        if ms.flags & LLMHF_INJECTED != 0 {
            return false;
        }
        let pt = Point::new(ms.pt.x, ms.pt.y);
        let ev = match classify_mouse(msg, ms.mouseData) {
            None => return false,
            Some(LowLevelMouse::Move) => {
                let mut st = self.lock();
                let base = if st.grabbed {
                    Some(st.park)
                } else {
                    st.last_pt
                };
                if !st.grabbed {
                    st.last_pt = Some(pt);
                }
                let delta = base.map_or(PointF::default(), |b| {
                    PointF::new(f64::from(pt.x - b.x), f64::from(pt.y - b.y))
                });
                CapturedEvent::MouseMove {
                    pos: pt.to_f(),
                    delta,
                }
            }
            Some(LowLevelMouse::Button { button, down }) => CapturedEvent::Button {
                button,
                down,
                pos: pt.to_f(),
            },
            Some(LowLevelMouse::Wheel { dx, dy }) => CapturedEvent::Wheel {
                dx,
                dy,
                unit: WheelUnit::Notch120,
            },
        };
        let is_move = matches!(ev, CapturedEvent::MouseMove { .. });
        let swallow = self.dispatch(ev, None);
        // A grabbed cursor stays parked whatever the sink said; a fail-safe
        // or the Engine's release has already cleared `grabbed`.
        swallow || (is_move && self.lock().grabbed)
    }
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32
        && let Some(s) = active()
    {
        // SAFETY: for HC_ACTION, lParam points to a KBDLLHOOKSTRUCT valid for this call.
        let kb = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        let swallow = panic::catch_unwind(AssertUnwindSafe(|| s.on_keyboard(wparam as u32, kb)))
            .unwrap_or_else(|_| {
                s.release_locally();
                s.notify_fault(CaptureFault::CallbackPanic);
                false
            });
        if swallow {
            return 1;
        }
    }
    // SAFETY: forwarding the unchanged hook arguments.
    unsafe { CallNextHookEx(ptr::null_mut(), code, wparam, lparam) }
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32
        && let Some(s) = active()
    {
        // SAFETY: for HC_ACTION, lParam points to an MSLLHOOKSTRUCT valid for this call.
        let ms = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
        let swallow = panic::catch_unwind(AssertUnwindSafe(|| s.on_mouse(wparam as u32, ms)))
            .unwrap_or_else(|_| {
                s.release_locally();
                s.notify_fault(CaptureFault::CallbackPanic);
                false
            });
        if swallow {
            return 1;
        }
    }
    // SAFETY: forwarding the unchanged hook arguments.
    unsafe { CallNextHookEx(ptr::null_mut(), code, wparam, lparam) }
}

/// Window procedure of the cursor hider: no cursor while over it.
unsafe extern "system" fn hider_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_SETCURSOR {
        // SAFETY: SetCursor(NULL) hides the cursor while it is over this window.
        unsafe { SetCursor(ptr::null_mut()) };
        return 1;
    }
    // SAFETY: default handling for every other message.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Creates the (hidden) cursor-hiding window on the calling thread.
fn create_hider() -> HWND {
    let class = wide("MacwinCursorHider");
    // SAFETY: plain Win32 window class registration and creation; the class
    // name buffer outlives both calls.
    unsafe {
        let hinstance = GetModuleHandleW(ptr::null());
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(hider_proc),
            hInstance: hinstance,
            lpszClassName: class.as_ptr(),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_NOACTIVATE,
            class.as_ptr(),
            ptr::null(),
            WS_POPUP,
            0,
            0,
            HIDER_SIZE,
            HIDER_SIZE,
            ptr::null_mut(),
            ptr::null_mut(),
            hinstance,
            ptr::null(),
        );
        if !hwnd.is_null() {
            // Alpha 1 instead of 0: fully transparent layered windows are
            // click-through and would never get WM_SETCURSOR.
            SetLayeredWindowAttributes(hwnd, 0, 1, LWA_ALPHA);
        }
        hwnd
    }
}

fn point_of(msg: &MSG) -> Point {
    Point::new(msg.wParam as isize as i32, msg.lParam as i32)
}

/// Puts the cursor (and the hider under it) on `p`.
///
/// # Safety
/// Must run on the capture thread, which owns `hider`.
unsafe fn park_cursor(hider: HWND, p: Point) {
    // SAFETY: plain Win32 calls on a window this thread created.
    unsafe {
        SetCursorPos(p.x, p.y);
        if !hider.is_null() {
            SetWindowPos(
                hider,
                HWND_TOPMOST,
                p.x - HIDER_SIZE / 2,
                p.y - HIDER_SIZE / 2,
                HIDER_SIZE,
                HIDER_SIZE,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            // Re-position so Windows sends WM_SETCURSOR to the hider.
            SetCursorPos(p.x, p.y);
        }
    }
}

fn run_hooks(shared: Arc<Shared>, ready: mpsc::Sender<Result<(), PlatformError>>) {
    // SAFETY: plain Win32 calls on this thread; the hook procedures only read
    // the hook structs Windows passes them.
    unsafe {
        shared
            .thread_id
            .store(GetCurrentThreadId(), Ordering::Release);
        *ACTIVE.lock().unwrap_or_else(|e| e.into_inner()) = Some(shared.clone());
        let hinstance = GetModuleHandleW(ptr::null());
        let kb: HHOOK = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), hinstance, 0);
        let ms: HHOOK = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), hinstance, 0);
        if kb.is_null() || ms.is_null() {
            if !kb.is_null() {
                UnhookWindowsHookEx(kb);
            }
            if !ms.is_null() {
                UnhookWindowsHookEx(ms);
            }
            *ACTIVE.lock().unwrap_or_else(|e| e.into_inner()) = None;
            let _ = ready.send(Err(PlatformError::Backend(
                "SetWindowsHookExW failed".into(),
            )));
            return;
        }
        let hider = create_hider();
        let timer = SetTimer(ptr::null_mut(), 0, 100, None);
        let _ = ready.send(Ok(()));
        info!("low-level hooks installed");
        let mut msg = MSG::default();
        loop {
            let r = GetMessageW(&mut msg, ptr::null_mut(), 0, 0);
            if r == 0 || r == -1 || msg.message == WM_QUIT {
                break;
            }
            match msg.message {
                WM_GRAB => {
                    let p = point_of(&msg);
                    park_cursor(hider, p);
                    shared.lock().park = p;
                }
                WM_RELEASE => {
                    if !hider.is_null() {
                        ShowWindow(hider, SW_HIDE);
                    }
                    let p = point_of(&msg);
                    SetCursorPos(p.x, p.y);
                }
                WM_REARM => {
                    if !hider.is_null() {
                        ShowWindow(hider, SW_HIDE);
                    }
                }
                WM_TIMER if msg.hwnd.is_null() => {
                    shared.check_lease();
                    if let Some(p) = shared.drifted_park() {
                        park_cursor(hider, p);
                    }
                }
                _ => {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
        if timer != 0 {
            KillTimer(ptr::null_mut(), timer);
        }
        UnhookWindowsHookEx(kb);
        UnhookWindowsHookEx(ms);
        if !hider.is_null() {
            DestroyWindow(hider);
        }
        *ACTIVE.lock().unwrap_or_else(|e| e.into_inner()) = None;
        info!("low-level hooks removed");
    }
}

/// Low-level hook capture backend. Only one can run per process.
#[derive(Default)]
pub struct WinCapture {
    thread: Option<JoinHandle<()>>,
    shared: Option<Arc<Shared>>,
}

impl WinCapture {
    pub fn new() -> WinCapture {
        WinCapture::default()
    }
}

struct WinControl {
    shared: Arc<Shared>,
}

impl CaptureControl for WinControl {
    fn command(&self, cmd: GrabCommand) {
        let mut st = self.shared.lock();
        match cmd {
            GrabCommand::Grab { exit } => {
                st.grabbed = true;
                st.drift_seen = false;
                st.park = st.last_pt.unwrap_or(exit);
                st.lease = Some(Instant::now() + INITIAL_LEASE);
                drop(st);
                self.shared.post(WM_GRAB, exit);
            }
            GrabCommand::Release { warp_to } => {
                st.grabbed = false;
                st.lease = None;
                st.last_pt = Some(warp_to);
                drop(st);
                self.shared.post(WM_RELEASE, warp_to);
            }
            GrabCommand::Rearm => {
                st.grabbed = false;
                st.lease = None;
                st.last_pt = None;
                drop(st);
                self.shared.post(WM_REARM, Point::default());
            }
        }
    }

    fn renew_lease(&self, until: Instant) {
        self.shared.lock().lease = Some(until);
    }

    fn stop(&self) {
        self.shared.release_locally();
        let tid = self.shared.thread_id.load(Ordering::Acquire);
        // SAFETY: asks our own capture thread's message loop to exit.
        unsafe {
            PostThreadMessageW(tid, WM_QUIT, 0, 0);
        }
    }
}

impl InputCapture for WinCapture {
    fn start(
        &mut self,
        sink: Arc<dyn CaptureSink>,
    ) -> Result<Arc<dyn CaptureControl>, PlatformError> {
        if self.thread.is_some() || active().is_some() {
            return Err(PlatformError::Backend("capture already started".into()));
        }
        let shared = Arc::new(Shared {
            sink,
            state: Mutex::new(State::default()),
            thread_id: AtomicU32::new(0),
        });
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread_shared = shared.clone();
        let handle = thread::Builder::new()
            .name("macwin-capture".into())
            .spawn(move || run_hooks(thread_shared, ready_tx))
            .map_err(|e| PlatformError::Backend(format!("cannot start the capture thread: {e}")))?;
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => {
                self.thread = Some(handle);
                self.shared = Some(shared.clone());
                Ok(Arc::new(WinControl { shared }))
            }
            Ok(Err(e)) => {
                let _ = handle.join();
                Err(e)
            }
            Err(_) => {
                let tid = shared.thread_id.load(Ordering::Acquire);
                if tid != 0 {
                    // SAFETY: asks our own capture thread's message loop to exit.
                    unsafe {
                        PostThreadMessageW(tid, WM_QUIT, 0, 0);
                    }
                }
                *ACTIVE.lock().unwrap_or_else(|e| e.into_inner()) = None;
                Err(PlatformError::Backend(
                    "the capture thread did not start".into(),
                ))
            }
        }
    }
}

impl Drop for WinCapture {
    fn drop(&mut self) {
        if let Some(shared) = &self.shared {
            WinControl {
                shared: shared.clone(),
            }
            .stop();
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
