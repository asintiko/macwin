//! The contract between the window and the application: the state the window
//! shows and the commands it sends back.

use macwin_core::arrange::{Placement, RectF};
use macwin_core::latency::{Report, SessionStats};
use macwin_core::types::Os;

use crate::i18n::Tr;

/// Interface language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Lang {
    /// Russian, the default.
    #[default]
    Ru,
    /// English.
    En,
}

/// What a computer does in the pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// Its keyboard and mouse control the other computer; it owns the
    /// screen arrangement.
    Primary,
    /// It is controlled from the primary one and shows the pairing code.
    Secondary,
}

/// Who is driving right now, while connected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Activity {
    /// Both computers are used locally.
    Idle,
    /// This computer's input goes to the peer.
    Controlling,
    /// The peer's input arrives here.
    Controlled,
}

/// The state of the connection, as one value the window can show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// The app is still starting up.
    Starting,
    /// A role is chosen but there is no peer.
    NotPaired,
    /// The primary has no screen arrangement yet.
    NeedsArrangement,
    /// The secondary waits for the primary; `code` is `None` until one is
    /// generated, then `"123-456"`.
    PairingWait {
        /// The pairing code to show.
        code: Option<String>,
    },
    /// The primary is looking for the secondary and pairing.
    PairingDial,
    /// Paired and waiting for the peer to connect.
    Listening,
    /// Paired and connecting to the peer.
    Connecting,
    /// The connection dropped; the reason is shown and a retry is scheduled.
    Retrying(String),
    /// Connected.
    Connected(Activity),
    /// Sharing is paused by the user.
    Paused,
    /// Sharing was paused by the emergency chord.
    EmergencyPaused,
    /// Unrecoverable error with a reason.
    Failed(String),
    /// The app is shutting down.
    Quitting,
}

/// The paired computer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerView {
    /// Its device name.
    pub name: String,
    /// Its operating system.
    pub os: Os,
}

/// One screen in logical units (macOS points, Windows DIPs).
#[derive(Debug, Clone, PartialEq)]
pub struct ScreenView {
    /// Stable id; a [`Placement`] refers to the primary's screens by it.
    pub id: u32,
    /// Position and size in the owner's logical space.
    pub rect: RectF,
    /// The main display.
    pub primary: bool,
    /// A laptop's own display.
    pub builtin: bool,
    /// The display name, if the system reports one.
    pub name: Option<String>,
}

/// User-changeable options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    /// Share the clipboard.
    pub clipboard: bool,
    /// Share pictures.
    pub images: bool,
    /// Share files and folders.
    pub files: bool,
    /// Swap Ctrl and Cmd/Win between the computers.
    pub remap: bool,
    /// Look for the peer on the local network.
    pub discovery: bool,
    /// Start at login; `None` when this build cannot do it.
    pub autostart: Option<bool>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            clipboard: true,
            images: true,
            files: true,
            remap: true,
            discovery: true,
            autostart: Some(false),
        }
    }
}

/// macOS privacy permissions the app needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Permissions {
    /// Accessibility (injecting input).
    pub accessibility: bool,
    /// Input monitoring (capturing input).
    pub input_monitoring: bool,
}

/// A permission whose system settings page can be opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Permission {
    /// Accessibility.
    Accessibility,
    /// Input monitoring.
    InputMonitoring,
}

/// A message for the user; dismissed with [`UiCommand::DismissNotice`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// Identifies the notice so a dismissal hits the right one.
    pub id: u64,
    /// A short headline.
    pub title: String,
    /// The details.
    pub message: String,
    /// Shown in the error style.
    pub error: bool,
}

/// Everything the window shows.
#[derive(Debug, Clone, PartialEq)]
pub struct UiState {
    /// Interface language.
    pub lang: Lang,
    /// The OS of this computer.
    pub this_os: Os,
    /// The name of this computer.
    pub device_name: String,
    /// The chosen role; `None` until the user picks one.
    pub role: Option<Role>,
    /// The connection state.
    pub status: Status,
    /// The paired computer, if any.
    pub peer: Option<PeerView>,
    /// This computer's screens, in logical units.
    pub local_screens: Vec<ScreenView>,
    /// The peer's screens from the last session; may be empty.
    pub peer_screens: Vec<ScreenView>,
    /// Primary: its own placement of the peer. Secondary: the primary's
    /// placement of this computer, relative to `peer_screens`.
    pub placement: Option<Placement>,
    /// User options.
    pub settings: Settings,
    /// macOS permissions; `None` on other systems.
    pub permissions: Option<Permissions>,
    /// A message waiting to be dismissed.
    pub notice: Option<Notice>,
    /// The app version.
    pub app_version: String,
    /// The current session's statistics; `None` while not connected.
    pub diagnostics: Option<Diagnostics>,
}

/// What the Diagnostics tab shows.
#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostics {
    pub stats: SessionStats,
    /// The peer's MacWin version, from its Hello.
    pub peer_version: String,
}

/// What the window asks the application to do. Sent through
/// [`Backend::send`], which must not block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiCommand {
    /// Choose the role of this computer.
    SetRole(Role),
    /// Secondary: show a pairing code.
    StartPairing,
    /// Primary: pair with the code `"123456"`, `"123-456"` or
    /// `"HOST[:PORT] CODE"`.
    EnterCode(String),
    /// Stop pairing, whichever side.
    CancelPairing,
    /// Primary: where the peer's screens are.
    SetPlacement(Placement),
    /// Pause or resume sharing.
    SetPaused(bool),
    /// Clipboard sharing options.
    SetClipboard {
        /// Share the clipboard at all.
        enabled: bool,
        /// Share pictures.
        images: bool,
        /// Share files and folders.
        files: bool,
    },
    /// Swap Ctrl and Cmd/Win.
    SetRemap(bool),
    /// Look for the peer on the local network.
    SetDiscovery(bool),
    /// Start at login.
    SetAutostart(bool),
    /// Interface language.
    SetLanguage(Lang),
    /// Forget the paired computer.
    Unpair,
    /// Open the system settings page of a permission.
    OpenPermissionSettings(Permission),
    /// Put text on this computer's clipboard (the diagnostics report).
    CopyText(String),
    /// Dismiss the notice with this id.
    DismissNotice(u64),
    /// Quit the app.
    Quit,
}

/// The application behind the window. All methods are called from the UI
/// thread except that other threads may call the waker.
pub trait Backend: Send + Sync + 'static {
    /// Changes whenever [`Backend::state`] does.
    fn version(&self) -> u64;
    /// A snapshot of the current state.
    fn state(&self) -> UiState;
    /// Hands a command to the application; must not block.
    fn send(&self, cmd: UiCommand);
    /// Registers a function the backend calls after each state change.
    fn set_waker(&self, waker: Box<dyn Fn() + Send + Sync>);
    /// The application has shut down: the window closes.
    fn finished(&self) -> bool;
}

impl UiState {
    /// The report "Copy report" puts on the clipboard (English, like
    /// `macwin probe latency`); `None` while not connected.
    pub fn diagnostics_report(&self) -> Option<String> {
        let d = self.diagnostics.as_ref()?;
        let os = |o| Tr(Lang::En).os_name(o);
        let this = format!(
            "\"{}\" ({}), MacWin {}",
            self.device_name,
            os(self.this_os),
            self.app_version
        );
        let other = self.peer.as_ref().map_or_else(String::new, |p| {
            format!("\"{}\" ({}), MacWin {}", p.name, os(p.os), d.peer_version)
        });
        let report = Report {
            connected_for: None,
            this: &this,
            other: &other,
            stats: Some(&d.stats),
            out_per_sec: d.stats.mouse_out_per_sec,
            in_per_sec: d.stats.mouse_in_per_sec,
            peak: false,
        };
        Some(report.to_string())
    }

    /// True when a peer is paired and the connection state says so.
    pub fn is_paired(&self) -> bool {
        self.peer.is_some()
            && !matches!(
                self.status,
                Status::Starting
                    | Status::NotPaired
                    | Status::NeedsArrangement
                    | Status::PairingWait { .. }
                    | Status::PairingDial
                    | Status::Quitting
            )
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    /// A paired primary Mac with the user's three-screen desk.
    pub fn desk_state() -> UiState {
        UiState {
            lang: Lang::Ru,
            this_os: Os::MacOs,
            device_name: "MacBook Pro".into(),
            role: Some(Role::Primary),
            status: Status::Connected(Activity::Idle),
            peer: Some(PeerView {
                name: "Dmitriy-PC".into(),
                os: Os::Windows,
            }),
            local_screens: desk_screens(),
            peer_screens: vec![ScreenView {
                id: 1,
                rect: RectF::new(0.0, 0.0, 1536.0, 864.0),
                primary: true,
                builtin: true,
                name: None,
            }],
            placement: None,
            settings: Settings::default(),
            permissions: None,
            notice: None,
            app_version: "0.1.0".into(),
            diagnostics: None,
        }
    }

    /// MacBook under a wide monitor that sticks out to the left, plus a
    /// second monitor to the right of the wide one.
    pub fn desk_screens() -> Vec<ScreenView> {
        vec![
            ScreenView {
                id: 1,
                rect: RectF::new(0.0, 0.0, 1512.0, 982.0),
                primary: true,
                builtin: true,
                name: None,
            },
            ScreenView {
                id: 2,
                rect: RectF::new(-524.0, -1440.0, 2560.0, 1440.0),
                primary: false,
                builtin: false,
                name: Some("LG UltraFine".into()),
            },
            ScreenView {
                id: 3,
                rect: RectF::new(2036.0, -1440.0, 1920.0, 1080.0),
                primary: false,
                builtin: false,
                name: Some("Dell U2720Q".into()),
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::desk_state;
    use super::*;

    #[test]
    fn the_copied_report_names_both_computers() {
        let mut st = desk_state();
        assert_eq!(st.diagnostics_report(), None);
        st.diagnostics = Some(Diagnostics {
            stats: SessionStats {
                mouse_out_per_sec: 120,
                ..SessionStats::default()
            },
            peer_version: "0.3.0".into(),
        });
        let text = st.diagnostics_report().unwrap();
        assert!(text.starts_with(
            "MacWin latency report\n\
             this computer:  \"MacBook Pro\" (macOS), MacWin 0.1.0\n\
             other computer: \"Dmitriy-PC\" (Windows), MacWin 0.3.0\n"
        ));
        assert!(text.contains("mouse datagrams: 120/s sent"), "{text}");
    }

    #[test]
    fn paired_needs_a_peer_and_a_settled_status() {
        let mut s = desk_state();
        assert!(s.is_paired());
        s.status = Status::PairingDial;
        assert!(!s.is_paired());
        s.status = Status::Retrying("x".into());
        assert!(s.is_paired());
        s.peer = None;
        assert!(!s.is_paired());
    }
}
