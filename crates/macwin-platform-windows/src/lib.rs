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
#[cfg(windows)]
mod capture;
#[cfg(windows)]
mod clipboard;
#[cfg(windows)]
mod dpapi;
#[cfg(windows)]
mod dpi;
#[cfg(windows)]
mod inject;
#[cfg(any(windows, test))]
mod lifecycle;
#[cfg(windows)]
pub mod registry;
#[cfg(windows)]
mod screens;
#[cfg(windows)]
pub mod tray;

#[cfg(windows)]
pub use capture::WinCapture;
#[cfg(windows)]
pub use clipboard::WinClipboard;
#[cfg(windows)]
pub use dpapi::DpapiStore;
#[cfg(windows)]
pub use dpi::init_dpi_awareness;
#[cfg(windows)]
pub use inject::WinInjector;
#[cfg(windows)]
pub use screens::WinScreens;
