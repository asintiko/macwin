//! Runs the MacWin window against a fake backend that simulates the app, so
//! the screens can be tried and photographed without the real agent.
//!
//! ```text
//! cargo run -p macwin-ui --example demo -- \
//!     --scenario role|arrange|code|wait-code|connected|secondary|settings \
//!     --lang ru|en --os mac|windows [--start-hidden] \
//!     [--placement left|dell-right|macbook-right|dell-top]
//! ```
//!
//! Entering the code `000-000` fails, any other code pairs. With
//! `--start-hidden` the window opens after three seconds, as if the tray icon
//! had been clicked.

use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use macwin_ui::{
    Activity, Backend, Edge, Lang, Notice, Os, PeerView, Permission, Permissions, Placement, RectF,
    Role, RunOptions, ScreenView, Settings, Status, UiCommand, UiState,
};

type Waker = Box<dyn Fn() + Send + Sync>;

struct Shared {
    state: Mutex<UiState>,
    version: AtomicU64,
    finished: AtomicBool,
    waker: Mutex<Option<Waker>>,
    notices: AtomicU64,
}

impl Shared {
    fn update(&self, f: impl FnOnce(&mut UiState)) {
        f(&mut self.state.lock().expect("state lock"));
        self.version.fetch_add(1, Ordering::SeqCst);
        if let Some(wake) = &*self.waker.lock().expect("waker lock") {
            wake();
        }
    }

    fn later(self: &Arc<Self>, millis: u64, f: impl FnOnce(&mut UiState) + Send + 'static) {
        let shared = Arc::clone(self);
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(millis));
            shared.update(f);
        });
    }
}

struct Demo(Arc<Shared>);

fn screen(
    id: u32,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    primary: bool,
    builtin: bool,
    name: Option<&str>,
) -> ScreenView {
    ScreenView {
        id,
        rect: RectF::new(x, y, w, h),
        primary,
        builtin,
        name: name.map(str::to_owned),
    }
}

/// The user's desk: a MacBook under a wide monitor, a second monitor beside it.
fn mac_desk() -> Vec<ScreenView> {
    vec![
        screen(1, 0.0, 0.0, 1512.0, 982.0, true, true, None),
        screen(
            2,
            -524.0,
            -1440.0,
            2560.0,
            1440.0,
            false,
            false,
            Some("LG UltraFine"),
        ),
        screen(
            3,
            2036.0,
            -1440.0,
            1920.0,
            1080.0,
            false,
            false,
            Some("Dell U2720Q"),
        ),
    ]
}

fn windows_laptop() -> Vec<ScreenView> {
    vec![screen(1, 0.0, 0.0, 1536.0, 864.0, true, true, None)]
}

fn left_of_macbook() -> Placement {
    Placement {
        display: Some(1),
        edge: Edge::Left,
        offset: Some(30),
    }
}

/// The starting arrangement named on the command line.
fn named_placement(name: &str) -> Placement {
    let (display, edge, offset) = match name {
        "dell-right" => (3, Edge::Right, 0),
        "macbook-right" => (1, Edge::Right, 100),
        "dell-top" => (3, Edge::Top, 300),
        _ => return left_of_macbook(),
    };
    Placement {
        display: Some(display),
        edge,
        offset: Some(offset),
    }
}

/// The computer on the other side of the pair.
fn peer_of(this: Os) -> (PeerView, Vec<ScreenView>) {
    match this {
        Os::MacOs => (
            PeerView {
                name: "Dmitriy-PC".into(),
                os: Os::Windows,
            },
            windows_laptop(),
        ),
        Os::Windows => (
            PeerView {
                name: "MacBook Pro".into(),
                os: Os::MacOs,
            },
            mac_desk(),
        ),
    }
}

fn initial(scenario: &str, lang: Lang, os: Os, placement: Placement) -> UiState {
    let (device_name, local_screens) = match os {
        Os::MacOs => ("MacBook Pro", mac_desk()),
        Os::Windows => ("Dmitriy-PC", windows_laptop()),
    };
    let mut st = UiState {
        lang,
        this_os: os,
        device_name: device_name.into(),
        role: None,
        status: Status::NotPaired,
        peer: None,
        local_screens,
        peer_screens: Vec::new(),
        placement: None,
        settings: Settings::default(),
        permissions: (os == Os::MacOs).then_some(Permissions {
            accessibility: true,
            input_monitoring: false,
        }),
        notice: None,
        app_version: env!("CARGO_PKG_VERSION").into(),
    };
    let pair = |st: &mut UiState, role: Role| {
        let (peer, screens) = peer_of(st.this_os);
        st.role = Some(role);
        st.peer = Some(peer);
        st.peer_screens = screens;
        st.placement = Some(placement);
        st.status = Status::Connected(Activity::Idle);
    };
    match scenario {
        "arrange" => {
            st.role = Some(Role::Primary);
            st.status = Status::NeedsArrangement;
        }
        "code" => {
            st.role = Some(Role::Primary);
            st.placement = Some(placement);
        }
        "wait-code" => {
            st.role = Some(Role::Secondary);
            st.status = Status::PairingWait {
                code: Some("482-913".into()),
            };
        }
        "connected" | "settings" => pair(&mut st, Role::Primary),
        "secondary" => pair(&mut st, Role::Secondary),
        _ => {}
    }
    st
}

impl Backend for Demo {
    fn version(&self) -> u64 {
        self.0.version.load(Ordering::SeqCst)
    }

    fn state(&self) -> UiState {
        self.0.state.lock().expect("state lock").clone()
    }

    fn send(&self, cmd: UiCommand) {
        eprintln!("command: {cmd:?}");
        let s = &self.0;
        match cmd {
            UiCommand::SetRole(role) => s.update(|st| {
                st.role = Some(role);
                st.status = if role == Role::Primary && st.placement.is_none() {
                    Status::NeedsArrangement
                } else {
                    Status::NotPaired
                };
            }),
            UiCommand::StartPairing => {
                s.update(|st| st.status = Status::PairingWait { code: None });
                s.later(1000, |st| {
                    if matches!(st.status, Status::PairingWait { code: None }) {
                        st.status = Status::PairingWait {
                            code: Some("482-913".into()),
                        };
                    }
                });
                s.later(9000, |st| {
                    if matches!(st.status, Status::PairingWait { code: Some(_) }) {
                        let (peer, screens) = peer_of(st.this_os);
                        st.peer = Some(peer);
                        st.peer_screens = screens;
                        st.placement = Some(left_of_macbook());
                        st.status = Status::Connected(Activity::Idle);
                    }
                });
            }
            UiCommand::EnterCode(text) => {
                s.update(|st| st.status = Status::PairingDial);
                let ok = !text.contains("000-000");
                let id = s.notices.fetch_add(1, Ordering::SeqCst) + 1;
                s.later(1500, move |st| {
                    if st.status != Status::PairingDial {
                        return;
                    }
                    if ok {
                        let (peer, screens) = peer_of(st.this_os);
                        st.peer = Some(peer);
                        st.peer_screens = screens;
                        st.status = Status::Connected(Activity::Idle);
                    } else {
                        st.status = Status::NotPaired;
                        st.notice = Some(Notice {
                            id,
                            title: "Неверный код".into(),
                            message: "Проверьте код на втором компьютере и попробуйте ещё раз."
                                .into(),
                            error: true,
                        });
                    }
                });
            }
            UiCommand::CancelPairing => s.update(|st| st.status = Status::NotPaired),
            UiCommand::SetPlacement(p) => {
                s.update(|st| {
                    st.placement = Some(p);
                    if st.peer.is_some() {
                        st.status = Status::Connecting;
                    }
                });
                s.later(700, |st| {
                    if st.status == Status::Connecting {
                        st.status = Status::Connected(Activity::Idle);
                    }
                });
            }
            UiCommand::SetPaused(paused) => s.update(|st| {
                st.status = if paused {
                    Status::Paused
                } else {
                    Status::Connected(Activity::Idle)
                };
            }),
            UiCommand::SetClipboard {
                enabled,
                images,
                files,
            } => s.update(|st| {
                st.settings.clipboard = enabled;
                st.settings.images = images;
                st.settings.files = files;
            }),
            UiCommand::SetRemap(v) => s.update(|st| st.settings.remap = v),
            UiCommand::SetDiscovery(v) => s.update(|st| st.settings.discovery = v),
            UiCommand::SetAutostart(v) => s.update(|st| st.settings.autostart = Some(v)),
            UiCommand::SetLanguage(lang) => s.update(|st| st.lang = lang),
            UiCommand::Unpair => s.update(|st| {
                st.peer = None;
                st.peer_screens.clear();
                st.status = Status::NotPaired;
            }),
            UiCommand::OpenPermissionSettings(which) => s.later(2000, move |st| {
                if let Some(p) = st.permissions.as_mut() {
                    match which {
                        Permission::Accessibility => p.accessibility = true,
                        Permission::InputMonitoring => p.input_monitoring = true,
                    }
                }
            }),
            UiCommand::DismissNotice(id) => s.update(|st| {
                if st.notice.as_ref().is_some_and(|n| n.id == id) {
                    st.notice = None;
                }
            }),
            UiCommand::Quit => {
                s.finished.store(true, Ordering::SeqCst);
                s.update(|st| st.status = Status::Quitting);
            }
        }
    }

    fn set_waker(&self, waker: Waker) {
        *self.0.waker.lock().expect("waker lock") = Some(waker);
    }

    fn finished(&self) -> bool {
        self.0.finished.load(Ordering::SeqCst)
    }
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let scenario = arg(&args, "--scenario").unwrap_or_else(|| "role".into());
    let lang = match arg(&args, "--lang").as_deref() {
        Some("en") => Lang::En,
        _ => Lang::Ru,
    };
    let os = match arg(&args, "--os").as_deref() {
        Some("windows") => Os::Windows,
        _ => Os::MacOs,
    };
    let start_hidden = args.iter().any(|a| a == "--start-hidden");
    let placement = named_placement(&arg(&args, "--placement").unwrap_or_default());

    let shared = Arc::new(Shared {
        state: Mutex::new(initial(&scenario, lang, os, placement)),
        version: AtomicU64::new(1),
        finished: AtomicBool::new(false),
        waker: Mutex::new(None),
        notices: AtomicU64::new(0),
    });
    let options = RunOptions {
        start_hidden,
        hide_on_close: start_hidden,
        on_start: Some(Box::new(move |control| {
            if start_hidden {
                thread::spawn(move || {
                    thread::sleep(Duration::from_secs(3));
                    eprintln!("tray: show the window");
                    control.show();
                });
            }
        })),
        on_visibility: Some(Box::new(|shown| eprintln!("visibility: {shown}"))),
    };
    match macwin_ui::run(Arc::new(Demo(shared)), options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("window failed: {e}");
            ExitCode::FAILURE
        }
    }
}
