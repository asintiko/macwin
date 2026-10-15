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
use macwin_core::types::{Os, RoleKind, Side};
use macwin_net::discovery::responder_addr;
use macwin_net::pairing::{PairInfo, PairedPeer, PairingCode};
use macwin_net::{Fingerprint, Identity, pair_connect, pair_listen};
use macwin_platform::PlatformError;
use macwin_platform::tray::Notice;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use super::{
    ID_AUTOSTART, ID_CLIPBOARD, ID_PAIR_CANCEL, ID_PAIR_CODE, ID_PAIR_WAIT, ID_PAUSE, ID_QUIT,
    ID_SIDE_LEFT, ID_SIDE_RIGHT, PeerSummary, Phase, TrayShared, TrayState, UiCommand,
};
use crate::autostart::{Autostart, AutostartState};
use crate::commands::{save_found_address, session_config};
use crate::config::{Config, normalize_address, side_name};
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
    let shared = Arc::new(TrayShared::new(ui_tx));
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
        self.start_sharing();
        while !self.quit {
            tokio::select! {
                command = ui.recv() => match command {
                    Some(UiCommand::Click(id)) => self.clicked(id).await,
                    Some(UiCommand::Entered(id, text)) => self.entered(id, text).await,
                    Some(UiCommand::Quit) | None => self.quit = true,
                },
                Some(event) = internal.recv() => self.internal(event),
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

    fn notify(&mut self, title: impl Into<String>, message: impl Into<String>) {
        self.notices += 1;
        self.state.notice = Some(Notice {
            id: self.notices,
            title: title.into(),
            message: message.into(),
        });
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

    fn fail(&mut self, message: String) {
        warn!(error = %message, "sharing stopped");
        self.paused = true;
        self.state.phase = Phase::Failed(message.clone());
        self.notify("Sharing stopped", message);
    }

    /// Starts the run loop when the config and the user allow it.
    fn start_sharing(&mut self) {
        let Some(config) = self.load_config() else {
            return;
        };
        let Some(peer) = config.peer.clone() else {
            return self.set_phase(Phase::NotPaired);
        };
        let Some(side) = peer.side else {
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
            Some(Placement::side(side)),
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
            return self.notify("Cannot change the setting", format!("{e:#}"));
        }
        if self.pairing.is_some() {
            self.load_config();
            return self.publish();
        }
        self.stop_sharing().await;
        self.start_sharing();
    }

    fn toggle_autostart(&mut self) {
        let result = match &self.env.autostart {
            None => return,
            Some(a) => match a.state() {
                Ok(AutostartState::On) => a.disable(),
                Ok(_) => a.enable(),
                Err(e) => Err(e),
            },
        };
        if let Err(e) = result {
            self.notify("Start at login", format!("{e:#}"));
        }
        self.refresh_autostart();
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
                return self.notify(
                    "Invalid pairing input",
                    format!(
                        "Enter a 6-digit code, or HOST[:PORT] followed by the code shown in the \
                         pairing notice. {e:#}"
                    ),
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
                        self.notify(
                            "Sharing paused",
                            "The emergency chord stopped sharing. Choose \"Resume sharing\" in \
                             the macwin menu to continue.",
                        );
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
                let title = if replacement {
                    format!("New pairing code {shown}")
                } else {
                    format!("Pairing code {shown}")
                };
                let prefix = if replacement {
                    "The previous code is no longer valid; use this replacement code.\n"
                } else {
                    ""
                };
                self.notify(
                    title,
                    format!(
                        "{prefix}On the other computer, choose Enter pairing code… in the macwin \
                         menu and type {digits}, or run:\n  macwin pair --code {digits}\n\
                         If LAN discovery fails, enter {address} {digits} in that menu or run:\n  \
                         macwin pair {address} --code {digits}\n\
                         The code works once, for 10 minutes."
                    ),
                );
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
            RunEvent::Connected { .. } => Phase::Connected(RoleKind::Idle),
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

    fn paired(&mut self, result: Result<(PairedPeer, Option<String>), String>) {
        match result {
            Ok((peer, address)) => {
                let (side, _) = agree_side(None, peer.peer_side);
                match store_pairing(&self.env.config_dir, &peer, side, address) {
                    Ok(_) => {
                        self.paused = false;
                        let message = match side {
                            Some(s) => format!(
                                "Its screens are on the {} of this computer. macwin now shares \
                                 the keyboard, mouse and clipboard with it.",
                                side_name(s)
                            ),
                            None => "Choose where its screens are: \"Other computer is on the \
                                     left\" or \"… on the right\" in the macwin menu."
                                .into(),
                        };
                        self.notify(format!("Paired with \"{}\"", peer.device_name), message);
                    }
                    Err(e) => self.notify("Pairing failed", format!("cannot save it: {e:#}")),
                }
            }
            Err(e) => self.notify("Pairing failed", e),
        }
        self.start_sharing();
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
