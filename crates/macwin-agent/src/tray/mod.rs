//! The tray (menu bar item on macOS, notification area icon on Windows):
//! the state it shows, how that state renders into a menu, and the
//! controller behind it. The OS layer in the platform crates only draws
//! [`TrayView`]s and reports clicks; everything here is testable without a
//! screen.

mod control;

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use macwin_core::types::{Os, RoleKind, Side};
use macwin_net::pairing::PairingCode;
use macwin_platform::tray::{
    MenuEntry, MenuItem, Notice, TextRequest, TrayApp, TrayIcon, TrayView,
};
use tokio::sync::mpsc;

pub use control::{DiscoveryTargets, MakeBackends, TrayEnv, start};

/// Pause or resume sharing.
pub const ID_PAUSE: u32 = 1;
/// Share the clipboard (on/off).
pub const ID_CLIPBOARD: u32 = 2;
/// The peer's screens are on the left.
pub const ID_SIDE_LEFT: u32 = 3;
/// The peer's screens are on the right.
pub const ID_SIDE_RIGHT: u32 = 4;
/// Wait for the other computer and show a code.
pub const ID_PAIR_WAIT: u32 = 5;
/// Enter the code shown on the other computer.
pub const ID_PAIR_CODE: u32 = 6;
pub const ID_PAIR_CANCEL: u32 = 7;
/// Start at login (on/off).
pub const ID_AUTOSTART: u32 = 8;
pub const ID_QUIT: u32 = 9;

/// The paired machine, as the menu shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerSummary {
    pub name: String,
    pub os: Os,
    pub side: Option<Side>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Starting,
    NotPaired,
    /// Paired, but where its screens are is unknown.
    NeedsSide,
    /// Waiting for the peer to connect.
    Listening,
    Connecting,
    /// A dial failed; the last error.
    Retrying(String),
    Connected(RoleKind),
    /// Paused by the user.
    Paused,
    /// Paused by the emergency chord.
    EmergencyPaused,
    /// Waiting for the other computer to enter the code (`None` until the
    /// listener is up).
    PairingWait {
        code: Option<PairingCode>,
    },
    /// Looking for the waiting computer and pairing with it.
    PairingDial,
    /// The run loop failed (for example: the port is taken).
    Failed(String),
    Quitting,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayState {
    pub phase: Phase,
    pub peer: Option<PeerSummary>,
    pub clipboard: bool,
    /// `None`: start at login is unavailable.
    pub autostart: Option<bool>,
    pub notice: Option<Notice>,
}

impl Default for TrayState {
    fn default() -> TrayState {
        TrayState {
            phase: Phase::Starting,
            peer: None,
            clipboard: true,
            autostart: None,
            notice: None,
        }
    }
}

fn os_title(os: Os) -> &'static str {
    match os {
        Os::MacOs => "Mac",
        Os::Windows => "Windows PC",
    }
}

/// `s` cut to `max` characters, with an ellipsis.
fn shorten(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

/// The first line of the menu (and the tooltip).
pub fn status_line(s: &TrayState) -> String {
    let peer = match &s.peer {
        Some(p) => format!("\"{}\"", p.name),
        None => "the other computer".into(),
    };
    match &s.phase {
        Phase::Starting => "Starting…".into(),
        Phase::NotPaired => "Not paired yet".into(),
        Phase::NeedsSide => format!("Where are the screens of {peer}?"),
        Phase::Listening => format!("Waiting for {peer}"),
        Phase::Connecting => format!("Connecting to {peer}…"),
        Phase::Retrying(_) => format!("Cannot reach {peer}; retrying"),
        Phase::Connected(RoleKind::Idle) => format!("Connected to {peer}"),
        Phase::Connected(RoleKind::Controlling) => format!("Controlling {peer}"),
        Phase::Connected(RoleKind::Controlled) => format!("{peer} is in control"),
        Phase::Paused => "Sharing paused".into(),
        Phase::EmergencyPaused => "Paused by the emergency chord".into(),
        Phase::PairingWait { code: Some(code) } => format!("Pairing code: {code}"),
        Phase::PairingWait { code: None } => "Getting ready to pair…".into(),
        Phase::PairingDial => "Pairing with the other computer…".into(),
        Phase::Failed(_) => "Sharing stopped by an error".into(),
        Phase::Quitting => "Quitting…".into(),
    }
}

fn icon(phase: &Phase) -> TrayIcon {
    match phase {
        Phase::Connected(_) => TrayIcon::Active,
        Phase::Starting
        | Phase::Listening
        | Phase::Connecting
        | Phase::Retrying(_)
        | Phase::PairingWait { .. }
        | Phase::PairingDial => TrayIcon::Waiting,
        Phase::Paused | Phase::EmergencyPaused | Phase::Quitting => TrayIcon::Paused,
        Phase::NotPaired | Phase::NeedsSide | Phase::Failed(_) => TrayIcon::Problem,
    }
}

/// The view of `s`: icon, tooltip, menu and notice.
pub fn render(s: &TrayState) -> TrayView {
    let status = status_line(s);
    let mut menu = vec![MenuEntry::Label(status.clone())];
    match &s.phase {
        Phase::Retrying(e) | Phase::Failed(e) => menu.push(MenuEntry::Label(shorten(e, 70))),
        Phase::NotPaired => menu.push(MenuEntry::Label(
            "Pair with the other computer to start".into(),
        )),
        Phase::NeedsSide => menu.push(MenuEntry::Label("Choose a side below".into())),
        Phase::PairingWait { code: Some(_) } => {
            menu.push(MenuEntry::Label("Enter it on the other computer".into()))
        }
        _ => {}
    }
    if let Some(p) = &s.peer {
        menu.push(MenuEntry::Label(format!(
            "Paired with \"{}\" ({})",
            shorten(&p.name, 40),
            os_title(p.os)
        )));
    }
    menu.push(MenuEntry::Separator);

    let quitting = s.phase == Phase::Quitting;
    let pairing = matches!(s.phase, Phase::PairingWait { .. } | Phase::PairingDial);
    let paused = matches!(
        s.phase,
        Phase::Paused | Phase::EmergencyPaused | Phase::Failed(_)
    );
    let side = s.peer.as_ref().and_then(|p| p.side);
    let can_share = side.is_some() && !pairing && !quitting;
    let pause_title = if paused {
        "Resume sharing"
    } else {
        "Pause sharing"
    };
    menu.push(MenuEntry::Item(
        MenuItem::new(ID_PAUSE, pause_title).enabled(can_share),
    ));
    menu.push(MenuEntry::Item(
        MenuItem::new(ID_CLIPBOARD, "Share clipboard")
            .enabled(!quitting)
            .checked(s.clipboard),
    ));
    if s.peer.is_some() {
        let editable = !pairing && !quitting;
        menu.push(MenuEntry::Item(
            MenuItem::new(ID_SIDE_LEFT, "Other computer is on the left")
                .enabled(editable)
                .checked(side == Some(Side::Left)),
        ));
        menu.push(MenuEntry::Item(
            MenuItem::new(ID_SIDE_RIGHT, "Other computer is on the right")
                .enabled(editable)
                .checked(side == Some(Side::Right)),
        ));
    }
    menu.push(MenuEntry::Separator);
    if pairing {
        menu.push(MenuEntry::Item(MenuItem::new(
            ID_PAIR_CANCEL,
            "Cancel pairing",
        )));
    } else {
        menu.push(MenuEntry::Item(
            MenuItem::new(ID_PAIR_WAIT, "Pair with another computer…").enabled(!quitting),
        ));
        menu.push(MenuEntry::Item(
            MenuItem::new(ID_PAIR_CODE, "Enter pairing code…")
                .enabled(!quitting)
                .input(TextRequest {
                    title: "Enter pairing code or address".into(),
                    message: "Type the code shown by the other computer. If LAN discovery fails, enter HOST:PORT and the code from its notice, for example 192.168.1.20:47800 123-456.".into(),
                    confirm: "Pair".into(),
                }),
        ));
    }
    menu.push(MenuEntry::Separator);
    if let Some(on) = s.autostart {
        menu.push(MenuEntry::Item(
            MenuItem::new(ID_AUTOSTART, "Start at login")
                .enabled(!quitting)
                .checked(on),
        ));
    }
    menu.push(MenuEntry::Item(
        MenuItem::new(ID_QUIT, "Quit macwin").enabled(!quitting),
    ));
    TrayView {
        icon: icon(&s.phase),
        tooltip: format!("macwin: {status}"),
        menu,
        notice: s.notice.clone(),
    }
}

/// Commands from the UI thread to the controller.
#[derive(Debug)]
enum UiCommand {
    Click(u32),
    Entered(u32, String),
    Quit,
}

/// What the UI thread shares with the controller: the published state and
/// the command channel.
pub struct TrayShared {
    published: Mutex<(u64, TrayState)>,
    commands: mpsc::UnboundedSender<UiCommand>,
    finished: AtomicBool,
}

impl TrayShared {
    fn new(commands: mpsc::UnboundedSender<UiCommand>) -> TrayShared {
        TrayShared {
            published: Mutex::new((0, TrayState::default())),
            commands,
            finished: AtomicBool::new(false),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, (u64, TrayState)> {
        self.published.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The current state.
    pub fn state(&self) -> TrayState {
        self.lock().1.clone()
    }

    /// Asks the controller to stop sharing and finish (Ctrl+C, logout).
    pub fn quit(&self) {
        let _ = self.commands.send(UiCommand::Quit);
    }

    fn publish(&self, state: &TrayState) {
        let mut p = self.lock();
        if p.1 != *state {
            p.0 += 1;
            p.1 = state.clone();
        }
    }

    fn set_finished(&self) {
        self.finished.store(true, Ordering::Release);
    }
}

impl TrayApp for TrayShared {
    fn version(&self) -> u64 {
        self.lock().0
    }

    fn view(&self) -> TrayView {
        render(&self.lock().1)
    }

    fn clicked(&self, id: u32) {
        let _ = self.commands.send(UiCommand::Click(id));
    }

    fn entered(&self, id: u32, text: String) {
        let _ = self.commands.send(UiCommand::Entered(id, text));
    }

    fn finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paired(phase: Phase, side: Option<Side>) -> TrayState {
        TrayState {
            phase,
            peer: Some(PeerSummary {
                name: "DESKTOP-1".into(),
                os: Os::Windows,
                side,
            }),
            clipboard: true,
            autostart: Some(false),
            notice: None,
        }
    }

    fn items(v: &TrayView) -> Vec<&MenuItem> {
        v.menu
            .iter()
            .filter_map(|e| match e {
                MenuEntry::Item(i) => Some(i),
                _ => None,
            })
            .collect()
    }

    fn item(v: &TrayView, id: u32) -> Option<&MenuItem> {
        items(v).into_iter().find(|i| i.id == id)
    }

    fn label(v: &TrayView) -> &str {
        match &v.menu[0] {
            MenuEntry::Label(l) => l,
            other => panic!("first entry {other:?}"),
        }
    }

    #[test]
    fn a_connected_tray_offers_pause_and_shows_the_side() {
        let v = render(&paired(
            Phase::Connected(RoleKind::Controlling),
            Some(Side::Right),
        ));
        assert_eq!(label(&v), "Controlling \"DESKTOP-1\"");
        assert_eq!(v.icon, TrayIcon::Active);
        assert_eq!(v.tooltip, "macwin: Controlling \"DESKTOP-1\"");
        let pause = item(&v, ID_PAUSE).unwrap();
        assert_eq!(
            (pause.title.as_str(), pause.enabled),
            ("Pause sharing", true)
        );
        assert!(item(&v, ID_SIDE_RIGHT).unwrap().checked);
        assert!(!item(&v, ID_SIDE_LEFT).unwrap().checked);
        assert!(item(&v, ID_CLIPBOARD).unwrap().checked);
        assert!(!item(&v, ID_AUTOSTART).unwrap().checked);
        assert!(item(&v, ID_PAIR_CANCEL).is_none());
        let code = item(&v, ID_PAIR_CODE).unwrap();
        assert!(code.input.is_some() && item(&v, ID_PAIR_WAIT).unwrap().input.is_none());
        assert!(items(&v).iter().all(|i| i.id != 0));
    }

    #[test]
    fn paused_states_offer_resume() {
        for phase in [
            Phase::Paused,
            Phase::EmergencyPaused,
            Phase::Failed("cannot listen".into()),
        ] {
            let v = render(&paired(phase.clone(), Some(Side::Left)));
            let pause = item(&v, ID_PAUSE).unwrap();
            assert_eq!(pause.title, "Resume sharing", "{phase:?}");
            assert!(pause.enabled);
        }
        let failed = render(&paired(Phase::Failed("port 47800 is taken".into()), None));
        assert_eq!(failed.icon, TrayIcon::Problem);
        assert!(
            failed
                .menu
                .contains(&MenuEntry::Label("port 47800 is taken".into()))
        );
    }

    #[test]
    fn without_a_peer_or_a_side_sharing_cannot_start() {
        let v = render(&TrayState {
            phase: Phase::NotPaired,
            ..TrayState::default()
        });
        assert_eq!(label(&v), "Not paired yet");
        assert_eq!(v.icon, TrayIcon::Problem);
        assert!(!item(&v, ID_PAUSE).unwrap().enabled);
        assert!(item(&v, ID_SIDE_LEFT).is_none(), "no peer, no side");
        assert!(item(&v, ID_AUTOSTART).is_none(), "unavailable");
        let v = render(&paired(Phase::NeedsSide, None));
        assert!(!item(&v, ID_PAUSE).unwrap().enabled);
        assert!(item(&v, ID_SIDE_LEFT).unwrap().enabled);
    }

    #[test]
    fn pairing_shows_the_code_and_only_cancel() {
        let code = PairingCode::parse("123456").unwrap();
        let v = render(&paired(
            Phase::PairingWait { code: Some(code) },
            Some(Side::Left),
        ));
        assert_eq!(label(&v), "Pairing code: 123-456");
        assert_eq!(v.icon, TrayIcon::Waiting);
        assert!(item(&v, ID_PAIR_CANCEL).is_some());
        assert!(item(&v, ID_PAIR_WAIT).is_none() && item(&v, ID_PAIR_CODE).is_none());
        assert!(!item(&v, ID_PAUSE).unwrap().enabled);
        assert!(!item(&v, ID_SIDE_LEFT).unwrap().enabled);
    }

    #[test]
    fn long_texts_are_shortened() {
        assert_eq!(shorten("abcdef", 3), "abc…");
        assert_eq!(shorten("abc", 3), "abc");
        assert_eq!(shorten("ééé", 2), "éé…");
    }

    #[test]
    fn the_shared_view_changes_version_only_on_change() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let shared = TrayShared::new(tx);
        let v0 = shared.version();
        let s = paired(Phase::Listening, Some(Side::Left));
        shared.publish(&s);
        let v1 = shared.version();
        shared.publish(&s);
        assert_eq!(shared.version(), v1);
        assert_ne!(v0, v1);
        assert_eq!(shared.view(), render(&s));
        shared.clicked(ID_PAUSE);
        shared.entered(ID_PAIR_CODE, "1".into());
        assert!(matches!(rx.try_recv(), Ok(UiCommand::Click(ID_PAUSE))));
        assert!(matches!(
            rx.try_recv(),
            Ok(UiCommand::Entered(ID_PAIR_CODE, _))
        ));
        assert!(!shared.finished());
        shared.set_finished();
        assert!(shared.finished());
    }
}
