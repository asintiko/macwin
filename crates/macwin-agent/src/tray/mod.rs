//! The tray (menu bar item on macOS, notification area icon on Windows):
//! the state it shows, how that state renders into a menu, and the
//! controller behind it. The OS layer in the platform crates only draws
//! [`TrayView`]s and reports clicks; everything here is testable without a
//! screen.
//!
//! With the app window, the same controller also serves the window: it
//! takes [`macwin_ui::UiCommand`]s and the window reads the published
//! [`TrayState`] (see `crate::window`). The menu then shrinks to "Open
//! MacWin", the status, pause and quit.

mod control;
pub mod text;

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use macwin_core::arrange::{self, Placement, RectF};
use macwin_core::geometry::ScreenSet;
use macwin_core::latency::SessionStats;
use macwin_core::types::{Os, RoleKind, ScreenInfo, Side};
use macwin_net::pairing::PairingCode;
use macwin_platform::tray::{
    MenuEntry, MenuItem, Notice, TextRequest, TrayApp, TrayIcon, TrayView,
};
use tokio::sync::mpsc;

pub use control::{DiscoveryTargets, MakeBackends, PermissionProbe, ScreenProbe, TrayEnv, start};

use crate::config::{Language, Role};
use text::Tr;

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
/// Show the app window (window-mode menu only).
pub const ID_OPEN: u32 = 10;

/// The paired machine, as the menu shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerSummary {
    pub name: String,
    pub os: Os,
    pub side: Option<Side>,
}

/// A screen in its owner's logical units (macOS points, Windows DIPs).
#[derive(Debug, Clone, PartialEq)]
pub struct ScreenDesc {
    pub id: u32,
    pub rect: RectF,
    pub primary: bool,
    /// A laptop's own display.
    pub builtin: bool,
}

/// The macOS privacy permissions sharing needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PermissionState {
    pub accessibility: bool,
    pub input_monitoring: bool,
}

/// Settings of `config.toml` the window can change, besides the clipboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub images: bool,
    pub files: bool,
    pub remap: bool,
    pub discovery: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            images: true,
            files: true,
            remap: true,
            discovery: true,
        }
    }
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

#[derive(Debug, Clone, PartialEq)]
pub struct TrayState {
    pub phase: Phase,
    pub peer: Option<PeerSummary>,
    pub clipboard: bool,
    /// `None`: start at login is unavailable.
    pub autostart: Option<bool>,
    pub notice: Option<Notice>,
    /// The notice reports a failure.
    pub notice_error: bool,
    /// This computer's role; `None` until it is chosen.
    pub role: Option<Role>,
    pub language: Language,
    /// Primary (or no role): where this computer puts the peer's screens.
    /// Secondary: where the primary put this computer, relative to
    /// `peer_screens`, as its last session announced.
    pub placement: Option<Placement>,
    pub options: Options,
    pub local_screens: Vec<ScreenDesc>,
    /// The peer's screens from the last session; empty before one.
    pub peer_screens: Vec<ScreenDesc>,
    /// `None` where the OS asks for no permissions.
    pub permissions: Option<PermissionState>,
    /// The peer's MacWin version, from its last Hello.
    pub peer_version: Option<String>,
}

impl Default for TrayState {
    fn default() -> TrayState {
        TrayState {
            phase: Phase::Starting,
            peer: None,
            clipboard: true,
            autostart: None,
            notice: None,
            notice_error: false,
            role: None,
            language: Language::default(),
            placement: None,
            options: Options::default(),
            local_screens: Vec::new(),
            peer_screens: Vec::new(),
            permissions: None,
            peer_version: None,
        }
    }
}

/// `screens` in logical units, as the arrangement editor shows them;
/// `builtin` tells a laptop's own display by its id.
pub fn describe_screens(screens: &[ScreenInfo], builtin: impl Fn(u32) -> bool) -> Vec<ScreenDesc> {
    let Ok(set) = ScreenSet::new(screens.to_vec()) else {
        return Vec::new();
    };
    arrange::logical_screens(&set)
        .into_iter()
        .map(|s| ScreenDesc {
            id: s.id,
            rect: s.rect,
            primary: s.primary,
            builtin: builtin(s.id),
        })
        .collect()
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
    render_with_legacy_side(s, true)
}

fn render_with_legacy_side(s: &TrayState, legacy_side_editable: bool) -> TrayView {
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
    let can_share = sharing_possible(s) && !pairing && !quitting;
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
    if s.peer.is_some() && legacy_side_editable {
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

/// A peer is paired and placed, so pause and resume mean something.
fn sharing_possible(s: &TrayState) -> bool {
    s.peer.is_some()
        && !matches!(
            s.phase,
            Phase::Starting | Phase::NotPaired | Phase::NeedsSide
        )
}

/// The menu next to the app window: the status, "Open MacWin", pause and
/// quit. Notices go to the window, so the tray shows none.
pub fn render_window(s: &TrayState) -> TrayView {
    let tr = Tr(s.language);
    let status = tr.status(s);
    let quitting = s.phase == Phase::Quitting;
    let pairing = matches!(s.phase, Phase::PairingWait { .. } | Phase::PairingDial);
    let paused = matches!(
        s.phase,
        Phase::Paused | Phase::EmergencyPaused | Phase::Failed(_)
    );
    let mut menu = vec![MenuEntry::Label(shorten(&status, 60))];
    if let Phase::Retrying(e) | Phase::Failed(e) = &s.phase {
        menu.push(MenuEntry::Label(shorten(e, 70)));
    }
    menu.push(MenuEntry::Separator);
    menu.push(MenuEntry::Item(
        MenuItem::new(ID_OPEN, tr.open()).enabled(!quitting),
    ));
    let pause = if paused { tr.resume() } else { tr.pause() };
    menu.push(MenuEntry::Item(
        MenuItem::new(ID_PAUSE, pause).enabled(sharing_possible(s) && !pairing && !quitting),
    ));
    menu.push(MenuEntry::Separator);
    menu.push(MenuEntry::Item(
        MenuItem::new(ID_QUIT, tr.quit()).enabled(!quitting),
    ));
    TrayView {
        icon: icon(&s.phase),
        tooltip: format!("MacWin: {status}"),
        menu,
        notice: None,
    }
}

/// Commands from the UI thread to the controller.
#[derive(Debug)]
enum UiCommand {
    Click(u32),
    Entered(u32, String),
    /// From the app window.
    Window(macwin_ui::UiCommand),
    Quit,
}

type Callback = Box<dyn Fn() + Send + Sync>;

/// What the UI thread shares with the controller: the published state and
/// the command channel.
pub struct TrayShared {
    published: Mutex<(u64, TrayState)>,
    /// The session statistics with their own version: they change four
    /// times a second, which only the window shows (the tray menu does not
    /// redraw for them).
    stats: Mutex<(u64, Option<SessionStats>)>,
    commands: mpsc::UnboundedSender<UiCommand>,
    finished: AtomicBool,
    legacy_side_editable: AtomicBool,
    /// The app window is up: the tray shows the window-mode menu.
    window: AtomicBool,
    /// Wakes the window after a change.
    waker: Mutex<Option<Callback>>,
    /// Shows the window.
    opener: Mutex<Option<Callback>>,
}

impl TrayShared {
    fn new(commands: mpsc::UnboundedSender<UiCommand>, window: bool) -> TrayShared {
        TrayShared {
            published: Mutex::new((0, TrayState::default())),
            stats: Mutex::new((0, None)),
            commands,
            finished: AtomicBool::new(false),
            legacy_side_editable: AtomicBool::new(false),
            window: AtomicBool::new(window),
            waker: Mutex::new(None),
            opener: Mutex::new(None),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, (u64, TrayState)> {
        self.published.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The current state.
    pub fn state(&self) -> TrayState {
        self.lock().1.clone()
    }

    /// The current session's statistics; `None` while not connected.
    pub fn stats(&self) -> Option<SessionStats> {
        self.stats.lock().unwrap_or_else(|e| e.into_inner()).1
    }

    /// Changes whenever [`TrayShared::stats`] does.
    pub fn stats_version(&self) -> u64 {
        self.stats.lock().unwrap_or_else(|e| e.into_inner()).0
    }

    fn set_stats(&self, stats: Option<SessionStats>) {
        let changed = {
            let mut s = self.stats.lock().unwrap_or_else(|e| e.into_inner());
            let changed = s.1 != stats;
            if changed {
                *s = (s.0 + 1, stats);
            }
            changed
        };
        if changed {
            self.wake();
        }
    }

    fn set_legacy_side_editable(&self, editable: bool) {
        if self.legacy_side_editable.swap(editable, Ordering::AcqRel) != editable {
            self.lock().0 += 1;
            self.wake();
        }
    }

    fn legacy_side_editable(&self) -> bool {
        self.legacy_side_editable.load(Ordering::Acquire)
    }

    /// Asks the controller to stop sharing and finish (Ctrl+C, logout).
    pub fn quit(&self) {
        let _ = self.commands.send(UiCommand::Quit);
    }

    /// Hands a command of the app window to the controller.
    pub fn send_window(&self, cmd: macwin_ui::UiCommand) {
        let _ = self.commands.send(UiCommand::Window(cmd));
    }

    /// Whether the tray serves an app window (the window-mode menu).
    pub fn window(&self) -> bool {
        self.window.load(Ordering::Acquire)
    }

    /// Switches to the classic menu when the window cannot open.
    pub fn set_window(&self, on: bool) {
        if self.window.swap(on, Ordering::AcqRel) != on {
            // A new version makes the tray redraw in the other style.
            self.lock().0 += 1;
        }
    }

    /// Called after every published change (from the controller's thread).
    pub fn set_waker(&self, waker: Callback) {
        *self.waker.lock().unwrap_or_else(|e| e.into_inner()) = Some(waker);
        self.wake();
    }

    /// What "Open MacWin" and a second launch do.
    pub fn set_opener(&self, opener: Callback) {
        *self.opener.lock().unwrap_or_else(|e| e.into_inner()) = Some(opener);
    }

    fn wake(&self) {
        if let Some(w) = &*self.waker.lock().unwrap_or_else(|e| e.into_inner()) {
            w();
        }
    }

    fn publish(&self, state: &TrayState) {
        let changed = {
            let mut p = self.lock();
            let changed = p.1 != *state;
            if changed {
                p.0 += 1;
                p.1 = state.clone();
            }
            changed
        };
        if changed {
            self.wake();
        }
    }

    fn set_finished(&self) {
        self.finished.store(true, Ordering::Release);
        self.wake();
    }
}

impl TrayApp for TrayShared {
    fn version(&self) -> u64 {
        self.lock().0
    }

    fn view(&self) -> TrayView {
        let window = self.window();
        let p = self.lock();
        if window {
            render_window(&p.1)
        } else {
            render_with_legacy_side(&p.1, self.legacy_side_editable())
        }
    }

    fn clicked(&self, id: u32) {
        if id == ID_OPEN {
            self.open();
            return;
        }
        let _ = self.commands.send(UiCommand::Click(id));
    }

    fn entered(&self, id: u32, text: String) {
        let _ = self.commands.send(UiCommand::Entered(id, text));
    }

    fn open(&self) -> bool {
        if !self.window() {
            return false;
        }
        match &*self.opener.lock().unwrap_or_else(|e| e.into_inner()) {
            Some(open) => {
                open();
                true
            }
            None => false,
        }
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
            ..TrayState::default()
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
    fn lifecycle_legacy_side_controls_follow_shared_availability() {
        let state = paired(Phase::Connected(RoleKind::Idle), Some(Side::Right));
        let (tx, _rx) = mpsc::unbounded_channel();
        let shared = TrayShared::new(tx, false);
        shared.publish(&state);
        shared.set_legacy_side_editable(true);
        assert!(item(&shared.view(), ID_SIDE_RIGHT).is_some());

        let version = shared.version();
        shared.set_legacy_side_editable(false);
        assert!(shared.version() > version);
        let v = shared.view();
        assert!(item(&v, ID_SIDE_LEFT).is_none());
        assert!(item(&v, ID_SIDE_RIGHT).is_none());
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
        let shared = TrayShared::new(tx, false);
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

    #[test]
    fn the_window_menu_opens_the_window_and_follows_the_language() {
        let mut s = paired(Phase::Connected(RoleKind::Idle), None);
        let v = render_window(&s);
        assert_eq!(label(&v), "Подключено к «DESKTOP-1»");
        assert_eq!(
            items(&v).iter().map(|i| i.id).collect::<Vec<_>>(),
            [ID_OPEN, ID_PAUSE, ID_QUIT]
        );
        assert!(
            item(&v, ID_PAUSE).unwrap().enabled,
            "a placed peer can pause"
        );
        assert_eq!(item(&v, ID_OPEN).unwrap().title, "Открыть MacWin…");
        s.language = Language::En;
        s.phase = Phase::Paused;
        s.notice = Some(Notice {
            id: 1,
            title: "t".into(),
            message: "m".into(),
        });
        let v = render_window(&s);
        assert_eq!(item(&v, ID_PAUSE).unwrap().title, "Resume");
        assert_eq!(v.notice, None, "the window shows notices");
        let v = render_window(&TrayState {
            phase: Phase::NotPaired,
            ..TrayState::default()
        });
        assert!(!item(&v, ID_PAUSE).unwrap().enabled);
    }

    #[test]
    fn the_shared_state_wakes_the_window_and_opens_it() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let shared = TrayShared::new(tx, true);
        let woken = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let w = woken.clone();
        shared.set_waker(Box::new(move || {
            w.fetch_add(1, Ordering::SeqCst);
        }));
        let after_set = woken.load(Ordering::SeqCst);
        shared.publish(&paired(Phase::Listening, None));
        assert_eq!(woken.load(Ordering::SeqCst), after_set + 1);
        assert!(!shared.open(), "no window yet");
        let opened = std::sync::Arc::new(AtomicBool::new(false));
        let o = opened.clone();
        shared.set_opener(Box::new(move || o.store(true, Ordering::SeqCst)));
        shared.clicked(ID_OPEN);
        assert!(opened.load(Ordering::SeqCst));
        assert!(
            rx.try_recv().is_err(),
            "opening is not a controller command"
        );
        assert_eq!(label(&shared.view()), "Ожидание «DESKTOP-1»");
        let v = shared.version();
        shared.set_window(false);
        assert_ne!(shared.version(), v, "the tray redraws in the classic style");
        assert_eq!(label(&shared.view()), "Waiting for \"DESKTOP-1\"");
        assert!(!shared.open());
    }
}
