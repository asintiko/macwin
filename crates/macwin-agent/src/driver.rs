//! Session driver: wires a capture backend, the Engine, a [`Link`] and an
//! injector together. Transport-agnostic (it only sees `macwin_net::link`
//! types), so Phase 0b's `spike kvm` plugs the real OS backends in and the
//! tests plug fakes in.
//!
//! Concurrency: one `Arc<Mutex<Core>>`. The capture thread locks it from
//! `CaptureSink::on_event`; the async loop locks it for link messages, ticks
//! and shutdown. Nothing awaits while holding the lock, and the lock order
//! is always Core -> backend internals.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use macwin_core::arrange::{Edge, Placement};
use macwin_core::engine::{
    Action, CloseReason, Engine, EngineConfig, PeerInfo, Timings, TransportRole,
};
use macwin_core::latency::SessionStats;
use macwin_core::msg::{
    CAP_CLIPBOARD_FILES, CAP_CLIPBOARD_IMAGE, CAP_CLIPBOARD_TEXT, CAP_INPUT_CAPTURE,
    CAP_INPUT_INJECT, CAP_WHEEL_PIXEL, Hello,
};
use macwin_core::types::{CaptureFault, CapturedEvent, Os, RoleKind, ScreenInfo, Side};
use macwin_net::codes;
use macwin_net::link::{Inbound, IncomingFiles, Link, LinkClosed, LinkSender};
use macwin_platform::{
    CaptureControl, CaptureSink, ClipboardBackend, InputCapture, InputInjector, PlatformError,
    ScreenSource,
};
use macwin_proto::PROTOCOL_VERSION;
use tokio::sync::{Notify, mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tracing::{debug, info, warn};

pub use crate::sharing::ClipboardSettings;
use crate::sharing::{Sharing, SharingEvents};

/// Static configuration of one KVM session.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub local_os: Os,
    pub os_version: String,
    pub device_name: String,
    /// Where I placed the peer's screens next to mine; `None` on a secondary
    /// computer, which follows the placement the primary announces.
    pub placement: Option<Placement>,
    /// Apply the Ctrl <-> Cmd/Win remap when this machine controls.
    pub remap: bool,
    pub timings: Timings,
    /// Share the clipboard (text, and images / files when enabled); `None`
    /// turns sharing off.
    pub clipboard: Option<ClipboardSettings>,
}

/// The OS (or fake) backends of one machine.
pub struct Backends {
    pub capture: Box<dyn InputCapture>,
    pub injector: Box<dyn InputInjector>,
    pub screens: Box<dyn ScreenSource>,
    /// Needed only when the session shares the clipboard.
    pub clipboard: Option<Box<dyn ClipboardBackend>>,
}

/// The "peer is to my left/right" that peers older than protocol 1.3 read
/// from a Hello: a placement centred on my outermost screen on that side.
fn legacy_side(p: Placement) -> Option<Side> {
    match (p.display, p.edge) {
        (None, Edge::Left) => Some(Side::Left),
        (None, Edge::Right) => Some(Side::Right),
        _ => None,
    }
}

/// The Hello a KVM agent sends: it can both control and be controlled
/// (with smooth scrolling), and shares its clipboard when configured to.
pub fn kvm_hello(cfg: &SessionConfig, screens: Vec<ScreenInfo>) -> Hello {
    let mut capabilities = vec![
        CAP_INPUT_CAPTURE.into(),
        CAP_INPUT_INJECT.into(),
        CAP_WHEEL_PIXEL.into(),
    ];
    if let Some(c) = &cfg.clipboard {
        capabilities.push(CAP_CLIPBOARD_TEXT.into());
        if c.images {
            capabilities.push(CAP_CLIPBOARD_IMAGE.into());
        }
        if c.files {
            capabilities.push(CAP_CLIPBOARD_FILES.into());
        }
    }
    Hello {
        protocol_version: PROTOCOL_VERSION,
        capabilities,
        os: cfg.local_os,
        os_version: cfg.os_version.clone(),
        device_name: cfg.device_name.clone(),
        screens,
        peer_side: cfg.placement.and_then(legacy_side),
        placement: cfg.placement,
        agent_version: env!("CARGO_PKG_VERSION").into(),
    }
}

/// What the Engine needs from the peer's Hello.
pub fn peer_info(h: &Hello) -> PeerInfo {
    PeerInfo {
        os: h.os,
        screens: h.screens.clone(),
        capabilities: h.capabilities.clone(),
        device_name: h.device_name.clone(),
        placement: h.placement,
    }
}

/// An established connection handed to the driver.
pub struct Connected {
    pub link: Link,
    pub peer: Hello,
    pub role: TransportRole,
}

/// Why a session ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEnd {
    PeerClosed,
    HeartbeatTimeout,
    ProtocolViolation(String),
    Shutdown,
    /// The emergency chord was pressed: local control is back and the agent
    /// should exit.
    EmergencyStop,
    Transport(String),
    Internal(String),
}

/// Asks a running session to stop; cheap to clone.
#[derive(Clone)]
pub struct Stopper(Arc<Notify>);

impl Stopper {
    /// Graceful stop (Leave to the peer, release, close).
    pub fn shutdown(&self) {
        self.0.notify_one();
    }
}

/// A running session. Dropping the handle requests shutdown; use
/// [`join`](Self::join) to wait for cleanup.
pub struct SessionHandle {
    /// The current role, updated after every Engine step.
    pub role: watch::Receiver<RoleKind>,
    /// Link and mouse statistics, updated with every datagram ping.
    pub stats: watch::Receiver<Option<SessionStats>>,
    shutdown: Arc<Notify>,
    link: LinkSender,
    join: JoinHandle<SessionEnd>,
}

impl SessionHandle {
    /// Asks for a graceful stop (Leave to the peer, release, close).
    pub fn shutdown(&self) {
        self.shutdown.notify_one();
    }

    /// A stop handle that outlives [`SessionHandle::join`].
    pub fn stopper(&self) -> Stopper {
        Stopper(self.shutdown.clone())
    }

    /// The session's link, for [`LinkSender::wait_closed`] before exiting.
    pub fn link(&self) -> LinkSender {
        self.link.clone()
    }

    /// Waits for the session to end.
    pub async fn join(mut self) -> SessionEnd {
        (&mut self.join)
            .await
            .unwrap_or_else(|e| SessionEnd::Internal(format!("driver task failed: {e}")))
    }

    pub fn is_finished(&self) -> bool {
        self.join.is_finished()
    }
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        self.shutdown.notify_one();
    }
}

struct Core {
    engine: Engine,
    injector: Box<dyn InputInjector>,
    capture: Option<Arc<dyn CaptureControl>>,
    link: LinkSender,
    role_tx: watch::Sender<RoleKind>,
    stats_tx: watch::Sender<Option<SessionStats>>,
    end: Option<SessionEnd>,
}

fn lock(core: &Mutex<Core>) -> MutexGuard<'_, Core> {
    // A panic elsewhere must not leave input grabbed: keep going.
    core.lock().unwrap_or_else(|e| e.into_inner())
}

fn end_for_reason(r: CloseReason) -> SessionEnd {
    match r {
        CloseReason::HeartbeatTimeout => SessionEnd::HeartbeatTimeout,
        CloseReason::ProtocolViolation(s) => SessionEnd::ProtocolViolation(s.to_string()),
        CloseReason::Shutdown => SessionEnd::Shutdown,
        CloseReason::Internal(s) => SessionEnd::Internal(s.to_string()),
    }
}

fn end_for_link(c: LinkClosed) -> SessionEnd {
    match c {
        LinkClosed::PeerClosed { .. } => SessionEnd::PeerClosed,
        LinkClosed::LocalClosed { code } => {
            SessionEnd::Internal(format!("link closed locally (code {code})"))
        }
        LinkClosed::TimedOut => SessionEnd::Transport("the connection timed out".into()),
        LinkClosed::Transport(s) => SessionEnd::Transport(s),
        LinkClosed::ProtocolViolation(s) => SessionEnd::ProtocolViolation(s),
    }
}

impl Core {
    fn set_end(&mut self, end: SessionEnd) {
        if self.end.is_none() {
            self.end = Some(end);
        }
    }

    /// Executes Engine actions in order.
    fn dispatch(&mut self, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::SendControl(m) => {
                    if let Err(e) = self.link.send_control(m) {
                        self.set_end(SessionEnd::Internal(format!(
                            "cannot send a control message: {e}"
                        )));
                    }
                }
                Action::SendInput(m) => {
                    if let Err(e) = self.link.send_input(m) {
                        self.set_end(SessionEnd::Internal(format!(
                            "cannot send an input message: {e}"
                        )));
                    }
                }
                Action::SendDatagram(m) => self.link.send_datagram(m),
                Action::Inject(cmd) => {
                    if let Err(e) = self.injector.inject(cmd) {
                        warn!(error = %e, "injection failed");
                    }
                }
                Action::Capture(cmd) => {
                    if let Some(c) = &self.capture {
                        c.command(cmd);
                    }
                }
                Action::RenewLease(until) => {
                    if let Some(c) = &self.capture {
                        c.renew_lease(until);
                    }
                }
                Action::Close(reason) => {
                    self.link.close(codes::code_for(&reason));
                    self.set_end(end_for_reason(reason));
                }
            }
        }
        self.role_tx.send_replace(self.engine.role());
    }
}

/// Starts clipboard sharing when both sides want it.
fn start_sharing(
    settings: Option<&ClipboardSettings>,
    backend: Option<Box<dyn ClipboardBackend>>,
    peer: &Hello,
    role: TransportRole,
    link: &LinkSender,
) -> Option<(Sharing, SharingEvents)> {
    let settings = settings?;
    let Some(backend) = backend else {
        warn!("clipboard sharing is off: no clipboard backend");
        return None;
    };
    if !peer.has_capability(CAP_CLIPBOARD_TEXT) {
        info!("clipboard sharing is off: the peer does not share its clipboard");
        return None;
    }
    match Sharing::start(settings, backend, peer, role, link.clone()) {
        Ok(started) => Some(started),
        Err(e) => {
            warn!(error = %e, "clipboard sharing is off: the clipboard is unavailable");
            None
        }
    }
}

/// The next item of an optional channel, or never.
async fn next_of<T>(rx: &mut Option<mpsc::Receiver<T>>) -> Option<T> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// A started capture backend and its control. The backend must live as long
/// as the session: real backends stop their capture thread when dropped.
struct Capture {
    control: Arc<dyn CaptureControl>,
    _backend: Box<dyn InputCapture>,
}

struct Sink {
    core: Arc<Mutex<Core>>,
    wake: Arc<Notify>,
}

impl CaptureSink for Sink {
    fn on_event(&self, ev: CapturedEvent) -> bool {
        let mut core = lock(&self.core);
        if core.end.is_some() {
            // The session is over: local input stays local.
            return true;
        }
        let out = core.engine.on_local_event(Instant::now(), ev);
        core.dispatch(out.actions);
        if core.end.is_some() {
            self.wake.notify_one();
        }
        out.deliver
    }

    fn on_fault(&self, fault: CaptureFault) {
        warn!(?fault, "capture fault");
        let mut core = lock(&self.core);
        let now = Instant::now();
        let actions = core.engine.on_capture_fault(now, fault);
        core.dispatch(actions);
        if fault == CaptureFault::EmergencyChord {
            // The user wants out: end the session cleanly (the peer releases
            // whatever we injected there) and let the agent exit. The reason is
            // recorded first so the Engine's Close(Shutdown) does not replace it.
            core.set_end(SessionEnd::EmergencyStop);
            let actions = core.engine.shutdown(now);
            core.dispatch(actions);
        }
        if core.end.is_some() {
            self.wake.notify_one();
        }
    }
}

/// Starts the session: reads the screens, builds the Engine, starts capture
/// and runs the driver loop on the current tokio runtime.
pub fn spawn_session(
    cfg: SessionConfig,
    backends: Backends,
    conn: Connected,
) -> Result<SessionHandle, PlatformError> {
    let Backends {
        mut capture,
        injector,
        screens,
        clipboard,
    } = backends;
    let Connected { link, peer, role } = conn;
    let Link { tx, rx, files } = link;
    let fail = |tx: &LinkSender, e: PlatformError| {
        tx.close_now(codes::INTERNAL);
        e
    };
    let local_screens = screens.screens().map_err(|e| fail(&tx, e))?;
    let engine = Engine::new(EngineConfig {
        local_os: cfg.local_os,
        local_screens,
        placement: cfg.placement,
        remap: cfg.remap,
        timings: cfg.timings,
    })
    .map_err(|e| {
        fail(
            &tx,
            PlatformError::Backend(format!("invalid local screens: {e}")),
        )
    })?;
    if cfg.placement.is_some() && peer.placement.is_some() {
        warn!(
            "both machines placed the other (both are set up as primary); this machine's own placement is used"
        );
    }
    let (role_tx, role_rx) = watch::channel(RoleKind::Idle);
    let (stats_tx, stats_rx) = watch::channel(None);
    let core = Arc::new(Mutex::new(Core {
        engine,
        injector,
        capture: None,
        link: tx.clone(),
        role_tx,
        stats_tx,
        end: None,
    }));
    let wake = Arc::new(Notify::new());
    let control = capture
        .start(Arc::new(Sink {
            core: core.clone(),
            wake: wake.clone(),
        }))
        .map_err(|e| fail(&tx, e))?;
    {
        let mut c = lock(&core);
        c.capture = Some(control.clone());
        let actions = c
            .engine
            .on_connected(Instant::now(), peer_info(&peer), role);
        c.dispatch(actions);
    }
    let sharing = start_sharing(cfg.clipboard.as_ref(), clipboard, &peer, role, &tx);
    let shutdown = Arc::new(Notify::new());
    // The run task owns the capture backend: dropping it stops capturing.
    let join = tokio::spawn(run(
        core,
        Channels { rx, files },
        cfg.timings,
        shutdown.clone(),
        wake,
        Capture {
            control,
            _backend: capture,
        },
        sharing,
    ));
    Ok(SessionHandle {
        role: role_rx,
        stats: stats_rx,
        shutdown,
        link: tx,
        join,
    })
}

/// What the link delivers.
struct Channels {
    rx: mpsc::Receiver<Inbound>,
    files: mpsc::Receiver<IncomingFiles>,
}

async fn run(
    core: Arc<Mutex<Core>>,
    channels: Channels,
    timings: Timings,
    shutdown: Arc<Notify>,
    wake: Arc<Notify>,
    capture: Capture,
    sharing: Option<(Sharing, SharingEvents)>,
) -> SessionEnd {
    let Channels { mut rx, files } = channels;
    let mut files = Some(files);
    let mut tick = tokio::time::interval(timings.tick);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let (mut sharing, mut local, mut transfers) = match sharing {
        Some((s, events)) => (Some(s), Some(events.local), Some(events.transfers)),
        None => (None, None, None),
    };
    loop {
        tokio::select! {
            content = next_of(&mut local) => match (content, sharing.as_mut()) {
                (Some(content), Some(s)) => s.on_local(content),
                _ => {
                    warn!("the clipboard thread stopped; clipboard sharing is off");
                    local = None;
                }
            },
            event = next_of(&mut transfers) => match (event, sharing.as_mut()) {
                (Some(event), Some(s)) => s.on_transfer(event),
                _ => transfers = None,
            },
            incoming = next_of(&mut files) => match (incoming, sharing.as_mut()) {
                (Some(incoming), Some(s)) => s.on_files(incoming),
                // Dropping the transfer stops the sender.
                (Some(_), None) => debug!("incoming files ignored: sharing is off"),
                (None, _) => files = None,
            },
            msg = rx.recv() => {
                let now = Instant::now();
                let mut c = lock(&core);
                match msg {
                    Some(Inbound::Clipboard(m)) => match sharing.as_mut() {
                        Some(s) => s.on_remote(m),
                        None => debug!("clipboard update ignored: sharing is off"),
                    },
                    Some(Inbound::Control(m)) => {
                        let a = c.engine.on_control(now, m);
                        c.dispatch(a);
                    }
                    Some(Inbound::Input(m)) => {
                        let a = c.engine.on_input(now, m);
                        c.dispatch(a);
                    }
                    Some(Inbound::Datagram(m)) => {
                        let a = c.engine.on_datagram(now, m);
                        c.dispatch(a);
                    }
                    Some(Inbound::Closed(reason)) => {
                        info!(?reason, "link closed");
                        let a = c.engine.on_disconnected(now);
                        c.dispatch(a);
                        c.set_end(end_for_link(reason));
                    }
                    None => {
                        let a = c.engine.on_disconnected(now);
                        c.dispatch(a);
                        c.set_end(SessionEnd::Transport("the link went away".into()));
                    }
                }
            }
            _ = tick.tick() => {
                let mut c = lock(&core);
                let now = Instant::now();
                let mut a = c.engine.on_tick(now);
                let ping = c.engine.poll_ping(now);
                let pinged = ping.is_some();
                a.extend(ping);
                c.dispatch(a);
                if pinged {
                    let stats = c.engine.stats(now);
                    c.stats_tx.send_replace(stats);
                }
            }
            () = shutdown.notified() => {
                let mut c = lock(&core);
                let a = c.engine.shutdown(Instant::now());
                c.dispatch(a);
                c.set_end(SessionEnd::Shutdown);
            }
            () = wake.notified() => {}
        }
        let mut c = lock(&core);
        if let Some(end) = c.end.clone() {
            // Make sure nothing stays injected or grabbed, whatever ended it.
            if c.engine.has_session() {
                let a = c.engine.on_disconnected(Instant::now());
                c.dispatch(a);
            }
            if !c.link.is_closed() {
                c.link.close_now(codes::INTERNAL);
            }
            // Core -> control -> backend state -> sink -> Core is a cycle:
            // break it so the session (and its connection) can be freed.
            c.capture = None;
            drop(c);
            capture.control.stop();
            let sharing = sharing.take();
            // Dropping the backends joins their threads: keep that off the
            // async workers.
            tokio::task::spawn_blocking(move || {
                drop(capture);
                drop(sharing);
            });
            info!(?end, "session ended");
            return end;
        }
    }
}
