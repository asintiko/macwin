//! Windows backend for macwin: low-level keyboard/mouse hooks, SendInput,
//! per-monitor-v2 DPI screen enumeration, the Win32 clipboard, DPAPI secret
//! storage, the login `Run` key and the notification area icon. Compiles to
//! an empty library on other operating systems.
//!
//! Known limits (by design of Windows):
//! - windows of elevated (admin) processes neither receive SendInput from a
//!   non-elevated process nor report input to its hooks (UIPI);
//! - the lock screen, UAC prompts and Ctrl+Alt+Del run on the secure
//!   desktop, unreachable without a SYSTEM service (Phase 4);
//! - Win+L and Ctrl+Alt+Del cannot be intercepted.
#![cfg(windows)]

mod capture;
mod clipboard;
mod dpapi;
mod dpi;
mod inject;
pub mod registry;
mod screens;
pub mod tray;

pub use capture::WinCapture;
pub use clipboard::WinClipboard;
pub use dpapi::DpapiStore;
pub use dpi::init_dpi_awareness;
pub use tray::signal_running_instance;
pub use inject::WinInjector;
pub use screens::WinScreens;
