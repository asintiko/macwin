//! `macwin-tray`: the MacWin app on Windows (window and notification area
//! icon). It is a GUI subsystem binary, so it opens no console window;
//! `--background` (start at login) starts it without the window.
#![cfg_attr(windows, windows_subsystem = "windows")]
#![forbid(unsafe_code)]

use std::process::ExitCode;

fn main() -> ExitCode {
    let background = std::env::args_os().skip(1).any(|a| a == "--background");
    match macwin_agent::commands::tray(background) {
        Ok(()) => ExitCode::SUCCESS,
        // Already logged and shown to the user.
        Err(_) => ExitCode::FAILURE,
    }
}
