//! The MacWin application window: a first-run wizard, the main window with
//! status and settings, and the drag-and-drop screen arrangement editor.
//!
//! The window knows nothing about the rest of the app. It reads a [`UiState`]
//! from a [`Backend`] and answers with [`UiCommand`]s.
//!
//! For screenshots and debugging, `MACWIN_UI_THEME=dark|light` overrides the
//! system theme.
#![forbid(unsafe_code)]

mod app;
mod canvas;
mod code;
mod editor;
mod flow;
mod glyphs;
mod groups;
mod home;
mod i18n;
mod icon;
mod model;
#[cfg(target_os = "macos")]
mod platform;
mod theme;
mod widgets;
mod wizard;

pub use app::{RunOptions, WindowControl, run};
pub use macwin_core::arrange::{Edge, Placement, RectF};
pub use macwin_core::types::Os;
pub use model::{
    Activity, Backend, Lang, Notice, PeerView, Permission, Permissions, Role, ScreenView, Settings,
    Status, UiCommand, UiState,
};
