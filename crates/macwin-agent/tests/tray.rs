//! The tray controller with fake backends over QUIC on 127.0.0.1: pairing
//! from the menu both ways, sharing, settings, pause, the emergency chord
//! and quitting.

use std::net::{SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use macwin_agent::autostart::{Autostart, LaunchAgentFile};
use macwin_agent::config::Config;
use macwin_agent::config::{Language, Role};
use macwin_agent::driver::{Backends, SessionConfig};
use macwin_agent::runner::{Mode, RunEvent, RunExit, RunOptions, run};
use macwin_agent::tray::{
    self, ID_AUTOSTART, ID_CLIPBOARD, ID_PAIR_CANCEL, ID_PAIR_CODE, ID_PAIR_WAIT, ID_PAUSE,
    ID_QUIT, ID_SIDE_LEFT, ID_SIDE_RIGHT, Phase, ScreenDesc, TrayEnv, TrayShared, TrayState,
};
use macwin_core::arrange::{Edge, Placement, RectF};
use macwin_core::engine::Timings;
use macwin_core::types::{CaptureFault, Os, Rect, ScreenInfo, Side};
use macwin_net::discovery::{Offer, Responder};
use macwin_net::pairing::{PairInfo, PairingCode};
use macwin_net::{Fingerprint, Identity, pair_connect, pair_listen};
use macwin_platform::PlatformError;
use macwin_platform::testing::{FakeCapture, FakeCaptureHandle, FakeInjector, FakeScreens};
use macwin_platform::tray::TrayApp;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

const T: Duration = Duration::from_secs(15);

fn free_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn local(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

type Captures = Arc<Mutex<Vec<FakeCaptureHandle>>>;

fn backends(captures: &Captures) -> Result<Backends, PlatformError> {
    let (capture, handle) = FakeCapture::new();
    captures.lock().unwrap().push(handle);
    Ok(Backends {
        capture: Box::new(capture),
        injector: Box::new(FakeInjector::new().0),
        screens: Box::new(FakeScreens(vec![ScreenInfo {
            id: 1,
            rect: Rect::new(0, 0, 1440, 900).unwrap(),
            native_per_logical: 1.0,
            primary: true,
        }])),
        clipboard: None,
    })
}

struct Tray {
    shared: Arc<TrayShared>,
    captures: Captures,
    task: JoinHandle<()>,
    id: Identity,
    listen: SocketAddr,
    dir: tempfile::TempDir,
    _home: tempfile::TempDir,
}

/// A tray on a Mac whose config listens on loopback and whose discovery
/// queries go to `targets`; the clipboard is off (fake backends have none).
fn start_tray(targets: Vec<SocketAddr>) -> Tray {
    start_tray_as(Os::MacOs, "mac", targets)
}

fn start_tray_as(os: Os, device_name: &str, targets: Vec<SocketAddr>) -> Tray {
    start_tray_with(os, device_name, targets, false)
}

/// `window`: an app window runs next to the tray (notices in Russian, the
/// window's commands).
fn start_tray_with(os: Os, device_name: &str, targets: Vec<SocketAddr>, window: bool) -> Tray {
    let dir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let listen = local(free_port());
    let mut config = Config {
        listen,
        discovery_port: free_port(),
        ..Config::default()
    };
    config.clipboard.enabled = false;
    config.save(dir.path()).unwrap();
    let captures: Captures = Arc::default();
    let made = captures.clone();
    let id = Identity::generate().unwrap();
    let env = TrayEnv {
        config_dir: dir.path().to_path_buf(),
        identity: id.clone(),
        os,
        device_name: device_name.into(),
        staging: dir.path().join("incoming"),
        make_backends: Arc::new(move |_clipboard| backends(&made)),
        autostart: Some(Autostart::new(
            Box::new(LaunchAgentFile::in_home(home.path())),
            "<plist/>".into(),
        )),
        discovery_targets: Arc::new(move |_port| targets.clone()),
        discovery_wait: Duration::from_millis(500),
        retry_min: Duration::from_millis(50),
        retry_max: Duration::from_millis(200),
        connect_timeout: Duration::from_millis(300),
        window,
        screens: Arc::new(|| {
            vec![ScreenDesc {
                id: 1,
                rect: RectF::new(0.0, 0.0, 1440.0, 900.0),
                primary: true,
                builtin: true,
            }]
        }),
        permissions: None,
    };
    let (shared, controller) = tray::start(env);
    Tray {
        shared,
        captures,
        task: tokio::spawn(controller),
        id,
        listen,
        dir,
        _home: home,
    }
}

impl Tray {
    async fn wait(&self, what: &str, want: impl Fn(&TrayState) -> bool) -> TrayState {
        let deadline = Instant::now() + T;
        loop {
            let s = self.shared.state();
            if want(&s) {
                return s;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: {s:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn config(&self) -> Config {
        Config::load(self.dir.path()).unwrap()
    }

    async fn quit(self) {
        self.shared.clicked(ID_QUIT);
        tokio::time::timeout(T, self.task)
            .await
            .expect("the controller did not finish")
            .unwrap();
        assert!(self.shared.finished());
        assert_eq!(self.shared.state().phase, Phase::Quitting);
    }
}

/// The other computer's run loop, dialing the tray.
struct Peer {
    events: mpsc::UnboundedReceiver<RunEvent>,
    stop: watch::Sender<bool>,
    task: JoinHandle<anyhow::Result<RunExit>>,
}

fn start_peer(id: Identity, tray: Fingerprint, addr: SocketAddr) -> Peer {
    let captures: Captures = Arc::default();
    let (tx, events) = mpsc::unbounded_channel();
    let (stop, stop_rx) = watch::channel(false);
    let opts = RunOptions {
        session: SessionConfig {
            local_os: Os::Windows,
            os_version: "test".into(),
            device_name: "pc".into(),
            placement: Some(Placement::side(Side::Left)),
            remap: true,
            timings: Timings::default(),
            clipboard: None,
        },
        mode: Mode::Dial(addr.to_string()),
        peer: tray,
        retry_min: Duration::from_millis(50),
        retry_max: Duration::from_millis(200),
        connect_timeout: Duration::from_millis(300),
        discovery: None,
    };
    let task = tokio::spawn(run(
        opts,
        id,
        move || backends(&captures),
        move |e| {
            let _ = tx.send(e);
        },
        stop_rx,
    ));
    Peer { events, stop, task }
}

impl Peer {
    async fn expect(&mut self, what: &str, want: impl Fn(&RunEvent) -> bool) {
        let deadline = tokio::time::Instant::now() + T;
        loop {
            let e = tokio::time::timeout_at(deadline, self.events.recv())
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
                .expect("the peer's run loop ended");
            if want(&e) {
                return;
            }
        }
    }

    async fn stop(self) {
        self.stop.send(true).unwrap();
        tokio::time::timeout(T, self.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}

fn connected(s: &TrayState) -> bool {
    matches!(s.phase, Phase::Connected(_))
}

fn notice_title(s: &TrayState) -> String {
    s.notice
        .as_ref()
        .map(|n| n.title.clone())
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn pairing_here_then_sharing_settings_pause_and_quit() {
    let tray = start_tray(vec![]);
    let s = tray
        .wait("not paired", |s| s.phase == Phase::NotPaired)
        .await;
    assert_eq!(s.autostart, Some(false));
    assert!(!s.clipboard);

    // Start at login switches on and off.
    tray.shared.clicked(ID_AUTOSTART);
    tray.wait("autostart on", |s| s.autostart == Some(true))
        .await;
    tray.shared.clicked(ID_AUTOSTART);
    tray.wait("autostart off", |s| s.autostart == Some(false))
        .await;

    // Pair: the tray waits and shows a code; the PC enters it.
    tray.shared.clicked(ID_PAIR_WAIT);
    let s = tray
        .wait("a code", |s| {
            matches!(s.phase, Phase::PairingWait { code: Some(_) })
        })
        .await;
    let Phase::PairingWait { code: Some(code) } = s.phase.clone() else {
        unreachable!()
    };
    assert_eq!(notice_title(&s), format!("Pairing code {code}"));
    assert!(
        s.notice.unwrap().message.contains(code.digits()),
        "the notice repeats the code"
    );
    let pc_id = Identity::generate().unwrap();
    let info = PairInfo {
        os: Os::Windows,
        device_name: "pc".into(),
        peer_side: Some(Side::Left),
    };
    let paired = pair_connect(local(0), tray.listen, &pc_id, &code, &info)
        .await
        .expect("the PC pairs with the code");
    assert_eq!(paired.fingerprint, tray.id.fingerprint());
    // The PC put the Mac on its left, so the PC is on the Mac's right.
    let s = tray
        .wait("listening for the new peer", |s| {
            s.phase == Phase::Listening
        })
        .await;
    let peer = s.peer.as_ref().unwrap();
    assert_eq!((peer.name.as_str(), peer.side), ("pc", Some(Side::Right)));
    assert_eq!(notice_title(&s), "Paired with \"pc\"");
    let stored = tray.config().peer.unwrap();
    assert_eq!(stored.fingerprint, pc_id.fingerprint());
    assert_eq!(stored.address, None, "the waiting side listens");

    // The PC connects; the tray shows it.
    let mut pc = start_peer(pc_id, tray.id.fingerprint(), tray.listen);
    pc.expect("connected", |e| matches!(e, RunEvent::Connected { .. }))
        .await;
    tray.wait("connected", connected).await;

    // A setting restarts the session with it.
    tray.shared.clicked(ID_CLIPBOARD);
    tray.wait("clipboard on", |s| s.clipboard).await;
    assert!(tray.config().clipboard.enabled);
    pc.expect("restart", |e| matches!(e, RunEvent::Ended(_)))
        .await;
    pc.expect("reconnected", |e| matches!(e, RunEvent::Connected { .. }))
        .await;
    tray.wait("connected again", connected).await;

    // The emergency chord pauses sharing; Resume brings it back.
    tray.captures
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .emit_fault(CaptureFault::EmergencyChord);
    let s = tray
        .wait("paused by the chord", |s| s.phase == Phase::EmergencyPaused)
        .await;
    assert_eq!(notice_title(&s), "Sharing paused");
    tray.shared.clicked(ID_PAUSE);
    tray.wait("resumed", connected).await;

    // Pause stops sharing until resumed.
    tray.shared.clicked(ID_PAUSE);
    tray.wait("paused", |s| s.phase == Phase::Paused).await;
    pc.expect("the tray left", |e| matches!(e, RunEvent::Ended(_)))
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(tray.shared.state().phase, Phase::Paused, "stays paused");

    tray.quit().await;
    pc.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn entering_a_code_finds_the_waiting_computer() {
    // The PC waits for pairing and answers discovery on loopback.
    let pc_id = Identity::generate().unwrap();
    let listener = pair_listen(local(0), &pc_id).await.unwrap();
    let pair_addr = listener.local_addr().unwrap();
    let responder = Responder::bind(
        local(0),
        Offer::Pairing {
            port: pair_addr.port(),
            os: Os::Windows,
            device_name: "pc".into(),
            fingerprint: pc_id.fingerprint(),
        },
    )
    .await
    .unwrap();
    let code = PairingCode::generate().unwrap();
    let waiting = {
        let code = code.clone();
        tokio::spawn(async move {
            let info = PairInfo {
                os: Os::Windows,
                device_name: "pc".into(),
                peer_side: None,
            };
            listener.accept(&code, &info).await
        })
    };
    let tray = start_tray(vec![responder.local_addr()]);
    tray.wait("not paired", |s| s.phase == Phase::NotPaired)
        .await;

    // Waiting can be cancelled.
    tray.shared.clicked(ID_PAIR_WAIT);
    tray.wait("waiting with a code", |s| {
        matches!(s.phase, Phase::PairingWait { code: Some(_) })
    })
    .await;
    tray.shared.clicked(ID_PAIR_CANCEL);
    let s = tray
        .wait("cancelled", |s| s.phase == Phase::NotPaired)
        .await;
    assert_eq!(s.notice, None);

    // A malformed code is refused before anything happens.
    tray.shared.entered(ID_PAIR_CODE, "12x".into());
    let s = tray
        .wait("refused", |s| notice_title(s) == "Invalid pairing input")
        .await;
    assert_eq!(s.phase, Phase::NotPaired);

    tray.shared.entered(ID_PAIR_CODE, code.to_string());
    let s = tray
        .wait("paired", |s| notice_title(s) == "Paired with \"pc\"")
        .await;
    assert_eq!(s.phase, Phase::NeedsSide, "nobody chose a side");
    assert!(s.notice.unwrap().message.contains("Choose where"));
    let peer = waiting.await.unwrap().expect("the PC paired too");
    assert_eq!(peer.fingerprint, tray.id.fingerprint());
    let stored = tray.config().peer.unwrap();
    assert_eq!(
        stored.address,
        Some(pair_addr.to_string()),
        "the dialing side"
    );

    // Choosing a side starts dialing.
    tray.shared.clicked(ID_SIDE_RIGHT);
    tray.wait("dialing", |s| {
        matches!(s.phase, Phase::Connecting | Phase::Retrying(_))
    })
    .await;
    assert_eq!(tray.config().peer.unwrap().side, Some(Side::Right));
    drop(responder);
    tray.quit().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn two_trays_pair_by_address_without_discovery_or_sides() {
    let mac = start_tray(vec![]);
    mac.wait("Mac not paired", |s| s.phase == Phase::NotPaired)
        .await;
    mac.shared.clicked(ID_PAIR_WAIT);
    let waiting = mac
        .wait("Mac pairing code", |s| {
            matches!(s.phase, Phase::PairingWait { code: Some(_) })
        })
        .await;
    let Phase::PairingWait { code: Some(code) } = waiting.phase else {
        unreachable!()
    };
    let direct_entry = format!("{} {}", mac.listen, code.digits());
    assert!(
        waiting
            .notice
            .as_ref()
            .unwrap()
            .message
            .contains(&direct_entry),
        "the notice explains the tray's direct-address fallback"
    );

    let windows = start_tray_as(Os::Windows, "windows", vec![]);
    windows
        .wait("Windows not paired", |s| s.phase == Phase::NotPaired)
        .await;
    windows.shared.entered(ID_PAIR_CODE, direct_entry);

    let mac_state = mac
        .wait("Mac paired without a configured side", |s| {
            s.phase == Phase::NeedsSide
        })
        .await;
    let windows_state = windows
        .wait("Windows paired without a configured side", |s| {
            s.phase == Phase::NeedsSide
        })
        .await;
    assert_eq!(
        (
            mac_state.peer.as_ref().unwrap().name.as_str(),
            mac_state.peer.as_ref().unwrap().os,
            mac_state.peer.as_ref().unwrap().side,
        ),
        ("windows", Os::Windows, None)
    );
    assert_eq!(
        (
            windows_state.peer.as_ref().unwrap().name.as_str(),
            windows_state.peer.as_ref().unwrap().os,
            windows_state.peer.as_ref().unwrap().side,
        ),
        ("mac", Os::MacOs, None)
    );
    assert_eq!(mac.config().peer.unwrap().address, None);
    assert_eq!(
        windows.config().peer.unwrap().address,
        Some(mac.listen.to_string())
    );

    windows.shared.clicked(ID_SIDE_LEFT);
    mac.shared.clicked(ID_SIDE_RIGHT);
    mac.wait("Mac connected", connected).await;
    windows.wait("Windows connected", connected).await;

    windows.quit().await;
    mac.quit().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_replacement_pairing_code_is_marked_and_cleared_on_cancel() {
    let tray = start_tray(vec![]);
    tray.wait("not paired", |s| s.phase == Phase::NotPaired)
        .await;
    tray.shared.clicked(ID_PAIR_WAIT);
    let first = tray
        .wait("initial pairing code", |s| {
            matches!(s.phase, Phase::PairingWait { code: Some(_) })
        })
        .await;
    let Phase::PairingWait {
        code: Some(first_code),
    } = first.phase
    else {
        unreachable!()
    };

    let wrong_code = loop {
        let code = PairingCode::generate().unwrap();
        if code != first_code {
            break code;
        }
    };
    let info = PairInfo {
        os: Os::Windows,
        device_name: "wrong-code-peer".into(),
        peer_side: None,
    };
    let attempt = tokio::time::timeout(
        T,
        pair_connect(
            local(0),
            tray.listen,
            &Identity::generate().unwrap(),
            &wrong_code,
            &info,
        ),
    )
    .await
    .expect("the wrong-code attempt should finish");
    assert!(attempt.is_err());

    let replacement = tray
        .wait("replacement pairing code", |s| {
            s.notice
                .as_ref()
                .is_some_and(|notice| notice.title.starts_with("New pairing code "))
        })
        .await;
    let Phase::PairingWait {
        code: Some(new_code),
    } = &replacement.phase
    else {
        unreachable!()
    };
    assert_eq!(
        notice_title(&replacement),
        format!("New pairing code {new_code}")
    );
    let new_digits = new_code.digits().to_owned();
    let notice = replacement.notice.unwrap();
    assert!(notice.message.contains("previous code is no longer valid"));
    assert!(notice.message.contains(&new_digits));

    tray.shared.clicked(ID_PAIR_CANCEL);
    let cancelled = tray
        .wait("cancelled", |s| s.phase == Phase::NotPaired)
        .await;
    assert_eq!(cancelled.notice, None);
    tray.quit().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn roles_pair_from_the_window_and_the_secondary_follows_the_arrangement() {
    use macwin_ui::{Lang, Role as UiRole, UiCommand};

    // The Windows laptop is the secondary: it shows a code.
    let pc = start_tray_with(Os::Windows, "pc", vec![], true);
    pc.wait("not paired", |s| s.phase == Phase::NotPaired).await;
    assert_eq!(
        pc.shared.state().local_screens.len(),
        1,
        "screens for the editor"
    );
    pc.shared.send_window(UiCommand::SetRole(UiRole::Secondary));
    pc.shared.send_window(UiCommand::StartPairing);
    let s = pc
        .wait("a code", |s| {
            matches!(s.phase, Phase::PairingWait { code: Some(_) })
        })
        .await;
    assert_eq!(s.role, Some(Role::Secondary));
    let Phase::PairingWait { code: Some(code) } = s.phase.clone() else {
        unreachable!()
    };
    assert!(
        notice_title(&s).starts_with("Адрес этого компьютера: 127.0.0.1:"),
        "the window shows the code; the notice gives the address: {s:?}"
    );

    // The MacBook is the primary: settings, the arrangement, then the code.
    let targets = vec![local(pc.config().discovery_port)];
    let mac = start_tray_with(Os::MacOs, "mac", targets, true);
    mac.wait("not paired", |s| s.phase == Phase::NotPaired)
        .await;
    mac.shared.send_window(UiCommand::SetRole(UiRole::Primary));
    mac.shared.send_window(UiCommand::SetLanguage(Lang::En));
    mac.shared.send_window(UiCommand::SetClipboard {
        enabled: false,
        images: false,
        files: true,
    });
    mac.shared.send_window(UiCommand::SetRemap(false));
    let placement = Placement {
        display: Some(1),
        edge: Edge::Left,
        offset: Some(100),
    };
    mac.shared.send_window(UiCommand::SetPlacement(placement));
    let s = mac
        .wait("primary, placed, in English", |s| {
            s.role == Some(Role::Primary)
                && s.placement == Some(placement)
                && s.language == Language::En
                && !s.options.remap
        })
        .await;
    assert!(!s.clipboard && !s.options.images && s.options.files);
    mac.shared
        .send_window(UiCommand::EnterCode(code.to_string()));

    // Both connect without another question; the secondary learns where the
    // primary put it.
    let s = mac.wait("connected", connected).await;
    assert_eq!(s.peer.as_ref().unwrap().name, "pc");
    assert_eq!(notice_title(&s), "Paired with \"pc\"");
    assert_eq!(s.peer_screens.len(), 1, "the peer's screens from its Hello");
    let s = pc
        .wait("connected with the arrangement", |s| {
            connected(s) && s.placement == Some(placement)
        })
        .await;
    assert_eq!(s.peer.as_ref().unwrap().name, "mac");
    assert_eq!(notice_title(&s), "Сопряжено с «mac»");
    assert_eq!(s.peer_screens[0].rect, RectF::new(0.0, 0.0, 1440.0, 900.0));
    let (mc, pcc) = (mac.config(), pc.config());
    assert_eq!(
        (mc.role, mc.arrangement),
        (Some(Role::Primary), Some(placement))
    );
    assert!(mc.peer.unwrap().address.is_some(), "the primary dials");
    assert_eq!(pcc.role, Some(Role::Secondary));
    assert_eq!(pcc.peer.unwrap().address, None, "the secondary listens");

    // Pause from the window, then resume.
    mac.shared.send_window(UiCommand::SetPaused(true));
    mac.wait("paused", |s| s.phase == Phase::Paused).await;
    mac.shared.send_window(UiCommand::SetPaused(false));
    mac.wait("connected again", connected).await;

    // Unpairing keeps the role and the arrangement.
    mac.shared.send_window(UiCommand::Unpair);
    let s = mac
        .wait("not paired", |s| s.phase == Phase::NotPaired)
        .await;
    assert!(s.peer.is_none() && s.peer_screens.is_empty());
    assert_eq!(s.placement, Some(placement));
    let mc = mac.config();
    assert!(mc.peer.is_none());
    assert_eq!(
        (mc.role, mc.arrangement),
        (Some(Role::Primary), Some(placement))
    );

    mac.quit().await;
    pc.quit().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_role_drops_the_placement_of_the_old_one() {
    use macwin_ui::{Role as UiRole, UiCommand};

    let mac = start_tray_with(Os::MacOs, "mac", vec![], true);
    mac.wait("not paired", |s| s.phase == Phase::NotPaired)
        .await;
    let placement = Placement {
        display: Some(1),
        edge: Edge::Top,
        offset: Some(0),
    };
    mac.shared.send_window(UiCommand::SetRole(UiRole::Primary));
    mac.shared.send_window(UiCommand::SetPlacement(placement));
    mac.wait("placed", |s| s.placement == Some(placement)).await;

    // A secondary shows only what its primary announces.
    mac.shared
        .send_window(UiCommand::SetRole(UiRole::Secondary));
    let s = mac
        .wait("secondary", |s| s.role == Some(Role::Secondary))
        .await;
    assert_eq!(s.placement, None);
    assert_eq!(mac.config().arrangement, Some(placement), "kept on disk");

    // Back to primary: its own arrangement again.
    mac.shared.send_window(UiCommand::SetRole(UiRole::Primary));
    mac.wait("primary, placed", |s| {
        s.role == Some(Role::Primary) && s.placement == Some(placement)
    })
    .await;
    mac.quit().await;
}
