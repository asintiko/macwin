//! The controller behind the tray: owns the run loop and the pairing flows
//! and turns menu choices into actions. It runs on the tokio runtime; the UI
//! thread only reads the published state and sends commands.

use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use macwin_core::arrange::Placement;
use macwin_core::types::{Os, RoleKind, ScreenInfo, Side};
use macwin_net::discovery::responder_addr;
use macwin_net::pairing::{PairInfo, PairedPeer, PairingCode};
use macwin_net::{Fingerprint, Identity, pair_connect, pair_listen};
use macwin_platform::PlatformError;
use macwin_platform::tray::Notice;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tracing::{info, warn};

use super::text::{Next, Tr};
use super::{
    ID_AUTOSTART, ID_CLIPBOARD, ID_PAIR_CANCEL, ID_PAIR_CODE, ID_PAIR_WAIT, ID_PAUSE, ID_QUIT,
    ID_SIDE_LEFT, ID_SIDE_RIGHT, Options, PeerSummary, PermissionState, Phase, ScreenDesc,
    TrayShared, TrayState, UiCommand, describe_screens,
};
use crate::autostart::{Autostart, AutostartState};
use crate::commands::{save_found_address, session_config};
use crate::config::{Config, Language, Role, normalize_address};
use crate::driver::Backends;
use crate::lan::reachable_ip;
use crate::pairing::{
    agree_side, find_waiting, pairing_responder, store_pairing, wait_for_pairing,
};
use crate::runner::{
    self, Mode, RunDiscovery, RunEvent, RunExit, RunOptions, bind_for, resolve, retry_in_use,
};

/// Builds the backends of one connection attempt; the flag says whether the
/// clipboard is shared.
pub type MakeBackends = Arc<dyn Fn(bool) -> Result<Backends, PlatformError> + Send + Sync>;
/// Where discovery queries go, for a discovery port.
pub type DiscoveryTargets = Arc<dyn Fn(u16) -> Vec<SocketAddr> + Send + Sync>;
/// This computer's screens, for the arrangement editor.
pub type ScreenProbe = Arc<dyn Fn() -> Vec<ScreenDesc> + Send + Sync>;
/// The macOS privacy permissions, checked without prompting.
pub type PermissionProbe = Arc<dyn Fn() -> PermissionState + Send + Sync>;

/// How often the screens and permissions are checked again: monitors come
/// and go, and permissions are granted in System Settings.
const ENV_POLL: Duration = Duration::from_secs(2);

/// Bound on waiting for a stopped run loop (its session drains for 2 s).
const STOP_WAIT: Duration = Duration::from_secs(5);

/// Everything the controller needs from the outside.
pub struct TrayEnv {
    pub config_dir: PathBuf,
    pub identity: Identity,
    pub os: Os,
    pub device_name: String,
    /// Where received files wait until they are pasted.
    pub staging: PathBuf,
    pub make_backends: MakeBackends,
    /// `None`: start at login is unavailable.
    pub autostart: Option<Autostart>,
    pub discovery_targets: DiscoveryTargets,
    pub discovery_wait: Duration,
    pub retry_min: Duration,
    pub retry_max: Duration,
    pub connect_timeout: Duration,
    /// An app window runs next to the tray: notices follow the language
    /// and point to the window.
    pub window: bool,
    pub screens: ScreenProbe,
    /// `None` where the OS asks for no permissions.
    pub permissions: Option<PermissionProbe>,
}

enum Internal {
    Run {
        tag: u64,
        event: RunEvent,
    },
    RunExit {
        tag: u64,
        result: Result<RunExit, String>,
    },
    PairCode {
        tag: u64,
        code: PairingCode,
        /// `HOST:PORT` of this machine, for typing on the other one.
        address: String,
        replacement: bool,
    },
    PairDone {
        tag: u64,
        /// The peer and, when this machine dialed, its address.
        result: Result<(PairedPeer, Option<String>), String>,
    },
}

struct Runner {
    tag: u64,
    task: JoinHandle<()>,
    stop: watch::Sender<bool>,
    dialing: bool,
    peer: Fingerprint,
}

struct Pairing {
    tag: u64,
    task: JoinHandle<()>,
}

struct Controller {
    env: TrayEnv,
    shared: Arc<TrayShared>,
    tx: mpsc::UnboundedSender<Internal>,
    /// Tags the events of every task, so a replaced one is ignored.
    tag: u64,
    runner: Option<Runner>,
    pairing: Option<Pairing>,
    /// Sharing stays off until the user resumes it.
    paused: bool,
    state: TrayState,
    notices: u64,
    quit: bool,
}

/// Creates the controller. Returns what the UI thread polls and the future
/// to spawn on the runtime; the future ends after a quit, once sharing has
/// stopped (then [`macwin_platform::tray::TrayApp::finished`] is true).
pub fn start(env: TrayEnv) -> (Arc<TrayShared>, impl Future<Output = ()> + Send + 'static) {
    let (ui_tx, ui_rx) = mpsc::unbounded_channel();
    let (tx, rx) = mpsc::unbounded_channel();
    let shared = Arc::new(TrayShared::new(ui_tx, env.window));
    let controller = Controller {
        env,
        shared: shared.clone(),
        tx,
        tag: 0,
        runner: None,
        pairing: None,
        paused: false,
        state: TrayState::default(),
        notices: 0,
        quit: false,
    };
    (shared, controller.run(ui_rx, rx))
}

fn set_side(c: &mut Config, side: Side) -> anyhow::Result<()> {
    c.peer.as_mut().context("not paired")?.side = Some(side);
    Ok(())
}

/// The placement the window shows: the primary's own (the legacy side
/// counts), or for a secondary the one its primary announced.
fn shown_placement(c: &Config, announced: Option<Placement>) -> Option<Placement> {
    match c.role {
        Some(Role::Secondary) => announced,
        _ => c.session_placement().ok().flatten(),
    }
}

/// Waits here for the other computer; `show` presents each code with this
/// machine's address.
async fn wait_here(
    config: &Config,
    id: &Identity,
    info: &PairInfo,
    mut show: impl FnMut(&PairingCode, String, bool),
) -> anyhow::Result<PairedPeer> {
    let listener = retry_in_use(|| pair_listen(config.listen, id))
        .await
        .with_context(|| format!("cannot listen on {}", config.listen))?;
    let local = listener.local_addr()?;
    let address = match reachable_ip(local) {
        Some(ip) => SocketAddr::new(ip, local.port()).to_string(),
        None => format!("<this computer's IP>:{}", local.port()),
    };
    let responder = match pairing_responder(config, local, info, id.fingerprint()).await {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "the other computer cannot find this one by itself");
            None
        }
    };
    let result = wait_for_pairing(&listener, info, |code, previous| {
        show(code, address.clone(), previous.is_some())
    })
    .await;
    drop(responder);
    listener.shutdown().await;
    result
}

/// Finds the computer that waits for pairing and pairs with it.
async fn pair_with_waiting(
    config: &Config,
    id: &Identity,
    info: &PairInfo,
    code: &PairingCode,
    targets: &[SocketAddr],
) -> anyhow::Result<(PairedPeer, Option<String>)> {
    let found = find_waiting(config, id.fingerprint(), targets).await?;
    info!(peer = %found.device_name, addr = %found.addr, "found the computer waiting for pairing");
    let peer = pair_connect(bind_for(found.addr), found.addr, id, code, info)
        .await
        .with_context(|| format!("pairing with \"{}\" failed", found.device_name))?;
    Ok((peer, Some(found.addr.to_string())))
}

fn parse_pair_entry(input: &str) -> anyhow::Result<(Option<String>, PairingCode)> {
    let input = input.trim();
    if let Ok(code) = PairingCode::parse(input) {
        return Ok((None, code));
    }
    let (address, code) = input
        .split_once(char::is_whitespace)
        .context("enter a pairing code or HOST[:PORT] followed by a code")?;
    Ok((
        Some(normalize_address(address)?),
        PairingCode::parse(code.trim())?,
    ))
}

async fn pair_at_address(
    address: &str,
    id: &Identity,
    info: &PairInfo,
    code: &PairingCode,
) -> anyhow::Result<(PairedPeer, Option<String>)> {
    let remote = resolve(address).await?;
    let peer = pair_connect(bind_for(remote), remote, id, code, info)
        .await
        .with_context(|| format!("pairing with {address} failed"))?;
    Ok((peer, Some(address.to_owned())))
}

impl Controller {
    async fn run(
        mut self,
        mut ui: mpsc::UnboundedReceiver<UiCommand>,
        mut internal: mpsc::UnboundedReceiver<Internal>,
    ) {
        self.refresh_autostart();
        self.refresh_environment();
        self.start_sharing();
        let mut poll = tokio::time::interval(ENV_POLL);
        poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
        while !self.quit {
            tokio::select! {
                command = ui.recv() => match command {
                    Some(UiCommand::Click(id)) => self.clicked(id).await,
                    Some(UiCommand::Entered(id, text)) => self.entered(id, text).await,
                    Some(UiCommand::Window(cmd)) => self.window_command(cmd).await,
                    Some(UiCommand::Quit) | None => self.quit = true,
                },
                Some(event) = internal.recv() => self.internal(event),
                _ = poll.tick() => self.refresh_environment(),
            }
        }
        self.state.phase = Phase::Quitting;
        self.publish();
        self.cancel_pairing();
        self.stop_sharing().await;
        self.shared.set_finished();
        info!("the tray stopped");
    }

    fn publish(&self) {
        self.shared.publish(&self.state);
    }

    fn set_phase(&mut self, phase: Phase) {
        self.state.phase = phase;
        self.publish();
    }

    /// The texts of the notices: the window follows the language, the
    /// classic menu stays English.
    fn tr(&self) -> Tr {
        Tr(if self.env.window {
            self.state.language
        } else {
            Language::En
        })
    }

    fn notify(&mut self, title: impl Into<String>, message: impl Into<String>) {
        self.show_notice(title.into(), message.into(), false);
    }

    fn notify_error(&mut self, title: impl Into<String>, message: impl Into<String>) {
        self.show_notice(title.into(), message.into(), true);
    }

    fn show_notice(&mut self, title: String, message: String, error: bool) {
        self.notices += 1;
        self.state.notice = Some(Notice {
            id: self.notices,
            title,
            message,
        });
        self.state.notice_error = error;
        self.publish();
    }

    /// Reads the config and shows its peer and clipboard setting.
    fn load_config(&mut self) -> Option<Config> {
        match Config::load(&self.env.config_dir) {
            Ok(c) => {
                self.state.peer = c.peer.as_ref().map(|p| PeerSummary {
                    name: p.name.clone(),
                    os: p.os,
                    side: p.side,
                });
                self.state.clipboard = c.clipboard.enabled;
                self.state.role = c.role;
                self.state.language = c.language;
                let announced = match c.role {
                    Some(Role::Secondary) => self.state.placement,
                    _ => None,
                };
                self.state.placement = shown_placement(&c, announced);
                self.state.options = Options {
                    images: c.clipboard.images,
                    files: c.clipboard.files,
                    remap: c.remap,
                    discovery: c.discovery,
                };
                if c.peer.is_none() {
                    self.state.peer_screens.clear();
                }
                Some(c)
            }
            Err(e) => {
                self.fail(format!("{e:#}"));
                None
            }
        }
    }

    fn refresh_autostart(&mut self) {
        self.state.autostart = match &self.env.autostart {
            None => None,
            Some(a) => match a.state() {
                Ok(s) => Some(s == AutostartState::On),
                Err(e) => {
                    warn!(error = %format!("{e:#}"), "cannot read the login item");
                    None
                }
            },
        };
        self.publish();
    }

    /// Checks the screens and the permissions again.
    fn refresh_environment(&mut self) {
        let screens = (self.env.screens)();
        let permissions = self.env.permissions.as_ref().map(|p| p());
        if screens != self.state.local_screens || permissions != self.state.permissions {
            self.state.local_screens = screens;
            self.state.permissions = permissions;
            self.publish();
        }
    }

    fn fail(&mut self, message: String) {
        warn!(error = %message, "sharing stopped");
        self.paused = true;
        self.state.phase = Phase::Failed(message.clone());
        let title = self.tr().sharing_stopped();
        self.notify_error(title, message);
    }

    /// Starts the run loop when the config and the user allow it.
    fn start_sharing(&mut self) {
        let Some(config) = self.load_config() else {
            return;
        };
        let Some(peer) = config.peer.clone() else {
            return self.set_phase(Phase::NotPaired);
        };
        // A secondary follows the placement its primary announces.
        let Ok(placement) = config.session_placement() else {
            return self.set_phase(Phase::NeedsSide);
        };
        if self.paused {
            if !matches!(self.state.phase, Phase::EmergencyPaused | Phase::Failed(_)) {
                self.state.phase = Phase::Paused;
            }
            return self.publish();
        }
        self.tag += 1;
        let tag = self.tag;
        let session = session_config(
            &config,
            self.env.os,
            self.env.device_name.clone(),
            placement,
            self.env.staging.clone(),
        );
        let with_clipboard = session.clipboard.is_some();
        let dialing = peer.address.is_some();
        let mode = match &peer.address {
            Some(a) => Mode::Dial(a.clone()),
            None => Mode::Listen(config.listen),
        };
        let discovery = config.discovery.then(|| RunDiscovery {
            responder: responder_addr(config.listen, config.discovery_port),
            targets: (self.env.discovery_targets)(config.discovery_port),
            wait: self.env.discovery_wait,
        });
        let opts = RunOptions {
            session,
            mode,
            peer: peer.fingerprint,
            retry_min: self.env.retry_min,
            retry_max: self.env.retry_max,
            connect_timeout: self.env.connect_timeout,
            discovery,
        };
        let (stop, stop_rx) = watch::channel(false);
        let make = self.env.make_backends.clone();
        let id = self.env.identity.clone();
        let tx = self.tx.clone();
        let task = tokio::spawn(async move {
            let events = tx.clone();
            let result = runner::run(
                opts,
                id,
                move || make(with_clipboard),
                move |event| {
                    let _ = events.send(Internal::Run { tag, event });
                },
                stop_rx,
            )
            .await;
            let result = result.map_err(|e| format!("{e:#}"));
            let _ = tx.send(Internal::RunExit { tag, result });
        });
        self.runner = Some(Runner {
            tag,
            task,
            stop,
            dialing,
            peer: peer.fingerprint,
        });
        self.set_phase(if dialing {
            Phase::Connecting
        } else {
            Phase::Listening
        });
    }

    async fn stop_sharing(&mut self) {
        if let Some(mut r) = self.runner.take() {
            let _ = r.stop.send(true);
            if tokio::time::timeout(STOP_WAIT, &mut r.task).await.is_err() {
                // A stuck run loop must not keep the ports, the capture or the session.
                warn!("the run loop did not stop in time; aborting it");
                r.task.abort();
            }
        }
    }

    fn cancel_pairing(&mut self) {
        if let Some(p) = self.pairing.take() {
            p.task.abort();
        }
    }

    async fn clicked(&mut self, id: u32) {
        match id {
            ID_PAUSE if self.pairing.is_none() => {
                if self.paused {
                    self.paused = false;
                    self.start_sharing();
                } else {
                    self.paused = true;
                    self.stop_sharing().await;
                    self.set_phase(Phase::Paused);
                }
            }
            ID_CLIPBOARD => {
                self.update_config(|c| {
                    c.clipboard.enabled = !c.clipboard.enabled;
                    Ok(())
                })
                .await
            }
            ID_SIDE_LEFT => self.update_config(|c| set_side(c, Side::Left)).await,
            ID_SIDE_RIGHT => self.update_config(|c| set_side(c, Side::Right)).await,
            ID_PAIR_WAIT => self.pair_here().await,
            ID_PAIR_CANCEL if self.pairing.is_some() => {
                self.cancel_pairing();
                self.state.notice = None;
                self.start_sharing();
            }
            ID_AUTOSTART => self.toggle_autostart(),
            ID_QUIT => self.quit = true,
            _ => {}
        }
    }

    async fn entered(&mut self, id: u32, text: String) {
        if id == ID_PAIR_CODE {
            self.pair_with_code(text).await;
        }
    }

    /// Changes the config and restarts sharing with it (pairing in
    /// progress is left alone).
    async fn update_config(&mut self, change: impl FnOnce(&mut Config) -> anyhow::Result<()>) {
        let dir = self.env.config_dir.clone();
        let result = Config::load(&dir).and_then(|mut c| {
            change(&mut c)?;
            c.save(&dir)
        });
        if let Err(e) = result {
            let title = self.tr().cannot_change_setting();
            return self.notify_error(title, format!("{e:#}"));
        }
        if self.pairing.is_some() {
            self.load_config();
            return self.publish();
        }
        self.stop_sharing().await;
        self.start_sharing();
    }

    fn toggle_autostart(&mut self) {
        let on = match &self.env.autostart {
            None => return,
            Some(a) => a.state().map(|s| s != AutostartState::On),
        };
        match on {
            Ok(on) => self.set_autostart(on),
            Err(e) => {
                let title = self.tr().autostart();
                self.notify_error(title, format!("{e:#}"));
            }
        }
    }

    fn set_autostart(&mut self, on: bool) {
        let result = match &self.env.autostart {
            None => return,
            Some(a) if on => a.enable(),
            Some(a) => a.disable(),
        };
        if let Err(e) = result {
            let title = self.tr().autostart();
            self.notify_error(title, format!("{e:#}"));
        }
        self.refresh_autostart();
    }

    async fn set_paused(&mut self, paused: bool) {
        if self.pairing.is_some() || paused == self.paused {
            return;
        }
        if paused {
            self.paused = true;
            self.stop_sharing().await;
            self.set_phase(Phase::Paused);
        } else {
            self.paused = false;
            self.start_sharing();
        }
    }

    /// Changes the config without touching a session (the language).
    fn save_setting(&mut self, change: impl FnOnce(&mut Config)) {
        let dir = self.env.config_dir.clone();
        let result = Config::load(&dir).and_then(|mut c| {
            change(&mut c);
            c.save(&dir)
        });
        if let Err(e) = result {
            let title = self.tr().cannot_change_setting();
            return self.notify_error(title, format!("{e:#}"));
        }
        self.load_config();
        self.publish();
    }

    /// A new role ends any pairing in progress; the peer stays until a new
    /// pairing replaces it.
    async fn set_role(&mut self, role: Role) {
        if self.pairing.is_some() {
            self.cancel_pairing();
            self.state.notice = None;
        }
        if self.state.role != Some(role) {
            // A primary shows its own arrangement, a secondary the one its
            // primary announces: the old role's means nothing to the new one.
            self.state.placement = None;
        }
        self.update_config(|c| {
            c.role = Some(role);
            Ok(())
        })
        .await;
    }

    /// Forgets the paired computer; role and arrangement stay.
    async fn unpair(&mut self) {
        self.cancel_pairing();
        self.stop_sharing().await;
        let dir = self.env.config_dir.clone();
        let result = Config::load(&dir).and_then(|mut c| {
            c.peer = None;
            c.save(&dir)
        });
        if let Err(e) = result {
            let title = self.tr().cannot_unpair();
            self.notify_error(title, format!("{e:#}"));
        } else {
            self.state.notice = None;
        }
        self.paused = false;
        self.state.peer_screens.clear();
        if self.state.role == Some(Role::Secondary) {
            self.state.placement = None;
        }
        self.start_sharing();
    }

    /// What the app window asks for.
    async fn window_command(&mut self, cmd: macwin_ui::UiCommand) {
        use macwin_ui::UiCommand as W;
        match cmd {
            W::SetRole(role) => self.set_role(role_of(role)).await,
            W::StartPairing => self.pair_here().await,
            W::EnterCode(text) => self.pair_with_code(text).await,
            W::CancelPairing => {
                if self.pairing.is_some() {
                    self.cancel_pairing();
                    self.state.notice = None;
                    self.start_sharing();
                }
            }
            W::SetPlacement(p) => {
                self.update_config(|c| {
                    c.arrangement = Some(p);
                    Ok(())
                })
                .await
            }
            W::SetPaused(paused) => self.set_paused(paused).await,
            W::SetClipboard {
                enabled,
                images,
                files,
            } => {
                self.update_config(|c| {
                    c.clipboard.enabled = enabled;
                    c.clipboard.images = images;
                    c.clipboard.files = files;
                    Ok(())
                })
                .await
            }
            W::SetRemap(on) => {
                self.update_config(|c| {
                    c.remap = on;
                    Ok(())
                })
                .await
            }
            W::SetDiscovery(on) => {
                self.update_config(|c| {
                    c.discovery = on;
                    Ok(())
                })
                .await
            }
            W::SetAutostart(on) => self.set_autostart(on),
            W::SetLanguage(lang) => self.save_setting(|c| c.language = language_of(lang)),
            W::Unpair => self.unpair().await,
            // The window opens System Settings itself, on its own thread.
            W::OpenPermissionSettings(_) => {}
            W::DismissNotice(id) => {
                if self.state.notice.as_ref().is_some_and(|n| n.id == id) {
                    self.state.notice = None;
                    self.publish();
                }
            }
            W::Quit => self.quit = true,
        }
    }

    fn pair_info(&self) -> PairInfo {
        PairInfo {
            os: self.env.os,
            device_name: self.env.device_name.clone(),
            peer_side: None,
        }
    }

    /// Waits for the other computer here and shows a code.
    async fn pair_here(&mut self) {
        self.cancel_pairing();
        self.stop_sharing().await;
        let Some(config) = self.load_config() else {
            return;
        };
        self.tag += 1;
        let tag = self.tag;
        let info = self.pair_info();
        let id = self.env.identity.clone();
        let tx = self.tx.clone();
        let task = tokio::spawn(async move {
            let codes = tx.clone();
            let show = move |code: &PairingCode, address: String, replacement| {
                let code = code.clone();
                let _ = codes.send(Internal::PairCode {
                    tag,
                    code,
                    address,
                    replacement,
                });
            };
            let result = wait_here(&config, &id, &info, show)
                .await
                .map(|peer| (peer, None))
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Internal::PairDone { tag, result });
        });
        self.pairing = Some(Pairing { tag, task });
        self.state.notice = None;
        self.set_phase(Phase::PairingWait { code: None });
    }

    /// Pairs by code, optionally at the address entered before it.
    async fn pair_with_code(&mut self, text: String) {
        let (address, code) = match parse_pair_entry(&text) {
            Ok(parsed) => parsed,
            Err(e) => {
                let tr = self.tr();
                return self.notify_error(
                    tr.invalid_pairing_input(),
                    tr.invalid_pairing_message(&format!("{e:#}")),
                );
            }
        };
        self.cancel_pairing();
        self.stop_sharing().await;
        let Some(config) = self.load_config() else {
            return;
        };
        self.tag += 1;
        let tag = self.tag;
        let info = self.pair_info();
        let id = self.env.identity.clone();
        let tx = self.tx.clone();
        let targets = if address.is_none() {
            (self.env.discovery_targets)(config.discovery_port)
        } else {
            Vec::new()
        };
        let task = tokio::spawn(async move {
            let result = match address {
                Some(address) => pair_at_address(&address, &id, &info, &code).await,
                None => pair_with_waiting(&config, &id, &info, &code, &targets).await,
            }
            .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Internal::PairDone { tag, result });
        });
        self.pairing = Some(Pairing { tag, task });
        self.state.notice = None;
        self.set_phase(Phase::PairingDial);
    }

    fn internal(&mut self, event: Internal) {
        let runner = self.runner.as_ref().map(|r| r.tag);
        let pairing = self.pairing.as_ref().map(|p| p.tag);
        match event {
            Internal::Run { tag, event } if runner == Some(tag) => self.run_event(event),
            Internal::RunExit { tag, result } if runner == Some(tag) => {
                self.runner = None;
                match result {
                    Ok(RunExit::Stopped) => {}
                    Ok(RunExit::EmergencyStop) => {
                        self.paused = true;
                        self.state.phase = Phase::EmergencyPaused;
                        let tr = self.tr();
                        self.notify(tr.paused_title(), tr.paused_by_chord(self.env.window));
                    }
                    Err(e) => self.fail(e),
                }
            }
            Internal::PairCode {
                tag,
                code,
                address,
                replacement,
            } if pairing == Some(tag) => {
                let (shown, digits) = (code.to_string(), code.digits().to_string());
                self.state.phase = Phase::PairingWait { code: Some(code) };
                let tr = self.tr();
                if self.env.window {
                    // The window shows the code; the notice adds the address.
                    let (title, message) = tr.address_notice(&address, replacement);
                    self.notify(title, message);
                } else {
                    self.notify(
                        tr.pairing_code_title(&shown, replacement),
                        tr.pairing_code_message(&digits, &address, replacement),
                    );
                }
            }
            Internal::PairDone { tag, result } if pairing == Some(tag) => {
                self.pairing = None;
                self.paired(result);
            }
            // From a run loop or a pairing that was stopped or replaced.
            _ => {}
        }
    }

    fn run_event(&mut self, event: RunEvent) {
        let (dialing, peer) = match &self.runner {
            Some(r) => (r.dialing, r.peer),
            None => return,
        };
        let phase = match event {
            RunEvent::Listening(_) => Phase::Listening,
            RunEvent::Dialing(_) => Phase::Connecting,
            RunEvent::Connected {
                screens, placement, ..
            } => {
                self.peer_connected(&screens, placement);
                Phase::Connected(RoleKind::Idle)
            }
            RunEvent::Role(role) => Phase::Connected(role),
            RunEvent::Ended(_) if dialing => Phase::Connecting,
            RunEvent::Ended(_) => Phase::Listening,
            RunEvent::Retrying { error, .. } if dialing => Phase::Retrying(error),
            // A stranger failed the handshake; still listening.
            RunEvent::Retrying { .. } => return,
            RunEvent::PeerFound(addr) => {
                save_found_address(&self.env.config_dir, peer, addr);
                return;
            }
        };
        self.set_phase(phase);
    }

    /// Remembers what the peer's Hello says about its screens and, for a
    /// secondary, where its primary put this computer.
    fn peer_connected(&mut self, screens: &[ScreenInfo], placement: Option<Placement>) {
        self.state.peer_screens = describe_screens(screens, |_| false);
        if self.state.role == Some(Role::Secondary) {
            self.state.placement = placement;
        }
    }

    fn paired(&mut self, result: Result<(PairedPeer, Option<String>), String>) {
        let tr = self.tr();
        match result {
            Ok((peer, address)) => {
                let (side, _) = agree_side(None, peer.peer_side);
                match store_pairing(&self.env.config_dir, &peer, side, address) {
                    Ok(_) => {
                        self.paused = false;
                        self.state.peer_screens.clear();
                        let next = match Config::load(&self.env.config_dir) {
                            Ok(c) if c.role.is_some() && c.session_placement().is_ok() => {
                                Next::Shares
                            }
                            _ => match side {
                                Some(s) => Next::OnSide(s),
                                None => Next::NeedsPlacement,
                            },
                        };
                        self.notify(
                            tr.paired(&peer.device_name),
                            tr.paired_message(next, self.env.window),
                        );
                    }
                    Err(e) => {
                        self.notify_error(tr.pairing_failed(), tr.cannot_save(&format!("{e:#}")))
                    }
                }
            }
            Err(e) => self.notify_error(tr.pairing_failed(), e),
        }
        self.start_sharing();
    }
}

fn role_of(role: macwin_ui::Role) -> Role {
    match role {
        macwin_ui::Role::Primary => Role::Primary,
        macwin_ui::Role::Secondary => Role::Secondary,
    }
}

fn language_of(lang: macwin_ui::Lang) -> Language {
    match lang {
        macwin_ui::Lang::Ru => Language::Ru,
        macwin_ui::Lang::En => Language::En,
    }
}

#[cfg(test)]
mod tests {
    use super::parse_pair_entry;

    #[test]
    fn pairing_entry_preserves_supported_code_formats() {
        for input in ["123456", "123-456", " 123 456 "] {
            let (address, code) = parse_pair_entry(input).unwrap();
            assert_eq!(address, None);
            assert_eq!(code.digits(), "123456");
        }
    }

    #[test]
    fn pairing_entry_accepts_an_address_and_code() {
        for input in ["127.0.0.1:47800 123-456", "127.0.0.1 123 456"] {
            let (address, code) = parse_pair_entry(input).unwrap();
            assert_eq!(address.as_deref(), Some("127.0.0.1:47800"));
            assert_eq!(code.digits(), "123456");
        }
    }

    #[test]
    fn pairing_entry_rejects_missing_or_extra_fields() {
        for input in ["", "123", "127.0.0.1", "127.0.0.1 123456 extra"] {
            assert!(parse_pair_entry(input).is_err(), "{input:?}");
        }
    }
}
