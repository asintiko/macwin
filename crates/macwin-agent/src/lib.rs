//! macwin agent library: session wiring between the network link, the core
//! Engine and the platform backends, shared by the `macwin` binary and tests.
#![forbid(unsafe_code)]

pub mod autostart;
pub mod cli;
pub mod commands;
pub mod driver;
pub mod logging;
pub mod platform;
pub mod spike_net;

use macwin_core::types::Os;

pub mod clipboard;
pub mod config;
pub mod identity_store;
pub mod instance;
pub mod lan;
pub mod pairing;
pub mod runner;
pub mod sharing;
pub mod transfer;
pub mod tray;
pub mod window;

/// The OS this binary runs on, if macwin supports it.
pub fn host_os() -> Option<Os> {
    if cfg!(target_os = "macos") {
        Some(Os::MacOs)
    } else if cfg!(windows) {
        Some(Os::Windows)
    } else {
        None
    }
}

/// A human-readable name of this machine (the macOS computer name, the
/// Windows computer name).
pub fn device_name() -> String {
    platform::device_name()
}
