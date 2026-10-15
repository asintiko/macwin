//! The menu bar item (macOS) / notification area icon (Windows): a view the
//! agent renders and the OS layer draws, and the callbacks through which the
//! OS layer reports the user's choices.
//!
//! The OS layer runs the UI loop on the main thread, polls
//! [`TrayApp::version`] every [`TRAY_POLL`] and redraws when it changed, so
//! nothing ever calls into the UI from another thread.

use std::time::Duration;

/// How often the OS layer polls the view.
pub const TRAY_POLL: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayIcon {
    /// Connected to the peer.
    Active,
    /// Waiting for, connecting to or pairing with the peer.
    Waiting,
    /// Sharing is paused.
    Paused,
    /// Needs attention: not paired, an error.
    Problem,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuEntry {
    /// A disabled line of text.
    Label(String),
    Separator,
    Item(MenuItem),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItem {
    /// Reported back through [`TrayApp::clicked`] / [`TrayApp::entered`];
    /// never 0.
    pub id: u32,
    pub title: String,
    pub enabled: bool,
    pub checked: bool,
    /// Ask for a line of text first and report it through
    /// [`TrayApp::entered`] (nothing is reported when the user cancels).
    pub input: Option<TextRequest>,
}

impl MenuItem {
    pub fn new(id: u32, title: impl Into<String>) -> MenuItem {
        MenuItem {
            id,
            title: title.into(),
            enabled: true,
            checked: false,
            input: None,
        }
    }

    pub fn enabled(mut self, enabled: bool) -> MenuItem {
        self.enabled = enabled;
        self
    }

    pub fn checked(mut self, checked: bool) -> MenuItem {
        self.checked = checked;
        self
    }

    pub fn input(mut self, request: TextRequest) -> MenuItem {
        self.input = Some(request);
        self
    }
}

/// A one-line text prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextRequest {
    pub title: String,
    pub message: String,
    /// The label of the confirming button.
    pub confirm: String,
}

/// A message shown once per id: an alert on macOS (closed again when the
/// app withdraws the notice), a notification on Windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub id: u64,
    pub title: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayView {
    pub icon: TrayIcon,
    pub tooltip: String,
    pub menu: Vec<MenuEntry>,
    pub notice: Option<Notice>,
}

/// The agent side of the tray. Called on the UI thread, so no method may
/// block.
pub trait TrayApp: Send + Sync {
    /// Changes whenever [`TrayApp::view`] does.
    fn version(&self) -> u64;
    fn view(&self) -> TrayView;
    fn clicked(&self, id: u32);
    fn entered(&self, id: u32, text: String);
    /// Shows the app's window, when it has one: the icon was clicked
    /// (Windows) or another launch asked for it. False when there is no
    /// window, so the OS layer falls back to the menu.
    fn open(&self) -> bool {
        false
    }
    /// True once the app has shut down; the UI loop then ends.
    fn finished(&self) -> bool;
}
