//! The real backends of the host OS (macOS or Windows).

use std::path::Path;
use std::sync::Arc;

use macwin_platform::{FileSecretStore, PlatformError, SecretStore};
use tracing::debug;

pub use imp::{
    alert, capture, clipboard, display_names, init, injector, open_permission_settings,
    permissions_report, request_permissions, run_tray, run_window, screens, show_running_instance,
};

use crate::driver::Backends;
use crate::identity_store::KeyStoreKind;
use crate::tray::{PermissionProbe, ScreenDesc, describe_screens};

/// This computer's screens for the arrangement editor; empty when the OS
/// cannot list them right now.
pub fn screen_descs() -> Vec<ScreenDesc> {
    match screens().screens() {
        Ok(list) => describe_screens(&list, imp::is_builtin),
        Err(e) => {
            debug!(error = %e, "cannot list the screens");
            Vec::new()
        }
    }
}

/// The privacy permissions to watch; `None` where the OS asks for none.
pub fn permission_probe() -> Option<PermissionProbe> {
    imp::permissions().map(|_| Arc::new(|| imp::permissions().unwrap_or(ALL_GRANTED)) as _)
}

/// What a missing check reports (it cannot fail on macOS).
const ALL_GRANTED: crate::tray::PermissionState = crate::tray::PermissionState {
    accessibility: true,
    input_monitoring: true,
};

/// All backends a KVM session needs; the clipboard only when it is shared.
pub fn backends(with_clipboard: bool) -> Result<Backends, PlatformError> {
    Ok(Backends {
        capture: capture(),
        injector: injector()?,
        screens: screens(),
        clipboard: with_clipboard.then(clipboard),
    })
}

/// The secret store of `kind`; file-based stores live in `config_dir`.
pub fn secret_store(
    kind: KeyStoreKind,
    config_dir: &Path,
) -> Result<Box<dyn SecretStore>, PlatformError> {
    match kind {
        KeyStoreKind::File => Ok(Box::new(FileSecretStore::new(config_dir))),
        KeyStoreKind::Keychain | KeyStoreKind::Dpapi => imp::os_secret_store(kind, config_dir),
    }
}

/// A human-readable name of this machine.
pub fn device_name() -> String {
    imp::device_name()
        .or_else(|| {
            ["COMPUTERNAME", "HOSTNAME"]
                .iter()
                .find_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()))
        })
        .unwrap_or_else(|| "unknown".into())
}

#[cfg(target_os = "macos")]
mod imp {
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Arc;

    use macwin_platform::tray::TrayApp;
    use macwin_platform::{
        ClipboardBackend, InputCapture, InputInjector, PlatformError, ScreenSource, SecretStore,
    };
    use macwin_platform_macos::app::{self, PrivacyPane};
    use macwin_platform_macos::{
        KeychainStore, MacCapture, MacClipboard, MacInjector, MacScreens, permissions, tray,
    };
    use macwin_ui::{Backend, Permission, RunOptions, WindowControl};
    use tracing::warn;

    use crate::identity_store::KeyStoreKind;
    use crate::tray::{PermissionState, TrayShared};

    pub fn init() {}

    /// The app window on the main thread with the menu bar item in its event
    /// loop, until the app has finished (then the process exits). Without a
    /// window (no OpenGL) the classic menu bar item takes over.
    pub fn run_window(shared: Arc<TrayShared>, backend: Arc<dyn Backend>, background: bool) {
        let tray_app: Arc<dyn TrayApp> = shared.clone();
        let opener = shared.clone();
        let options = RunOptions {
            start_hidden: background,
            hide_on_close: true,
            on_start: Some(Box::new(move |window: WindowControl| {
                let show = window.clone();
                opener.set_opener(Box::new(move || show.show()));
                if let Err(e) = tray::install(tray_app) {
                    warn!(error = %e, "cannot add the menu bar item");
                }
                // Finder, the Dock or `open` while MacWin runs.
                app::on_reopen(Box::new(move || window.show()));
            })),
            on_visibility: Some(Box::new(|shown| {
                app::set_dock_icon(shown);
                if shown {
                    app::activate();
                }
            })),
        };
        if let Err(e) = macwin_ui::run(backend, options) {
            warn!(error = %e, "cannot open the window; the menu bar item goes on alone");
            shared.set_window(false);
            if let Err(e) = tray::run(shared) {
                warn!(error = %e, "the menu bar item failed");
            }
        }
    }

    /// LaunchServices hands a second launch to the running app (a reopen).
    pub fn show_running_instance() -> bool {
        false
    }

    pub fn is_builtin(display: u32) -> bool {
        app::is_builtin(display)
    }

    pub fn permissions() -> Option<PermissionState> {
        let p = permissions::check();
        Some(PermissionState {
            accessibility: p.accessibility,
            input_monitoring: p.listen_events,
        })
    }

    /// Main thread only (the window's state is read there).
    pub fn display_names() -> HashMap<u32, String> {
        app::display_names()
    }

    /// Shows the system prompts for Accessibility and Input Monitoring while
    /// either is missing; only then does System Settings list MacWin. macOS
    /// shows each prompt once per app signature.
    pub fn request_permissions() {
        let p = permissions::check();
        if !(p.sufficient() && p.listen_events) {
            let _ = permissions::request();
        }
    }

    pub fn open_permission_settings(p: Permission) {
        let pane = match p {
            Permission::Accessibility => PrivacyPane::Accessibility,
            Permission::InputMonitoring => PrivacyPane::InputMonitoring,
        };
        if !app::open_privacy_settings(pane) {
            warn!(?pane, "cannot open System Settings");
        }
    }

    /// The menu bar item; on the main thread, until the app has finished.
    pub fn run_tray(app: Arc<dyn TrayApp>) -> Result<(), PlatformError> {
        tray::run(app)
    }

    pub fn alert(title: &str, message: &str) {
        tray::alert(title, message);
    }

    pub fn screens() -> Box<dyn ScreenSource> {
        Box::new(MacScreens)
    }

    pub fn capture() -> Box<dyn InputCapture> {
        Box::new(MacCapture::new())
    }

    pub fn injector() -> Result<Box<dyn InputInjector>, PlatformError> {
        Ok(Box::new(MacInjector::new()?))
    }

    pub fn clipboard() -> Box<dyn ClipboardBackend> {
        Box::new(MacClipboard)
    }

    pub fn device_name() -> Option<String> {
        macwin_platform_macos::device_name()
    }

    pub fn os_secret_store(
        kind: KeyStoreKind,
        _config_dir: &Path,
    ) -> Result<Box<dyn SecretStore>, PlatformError> {
        match kind {
            KeyStoreKind::Keychain => Ok(Box::new(KeychainStore::new())),
            _ => Err(PlatformError::Unsupported),
        }
    }

    /// `prompt` asks macOS to show the permission prompts first.
    pub fn permissions_report(prompt: bool) -> String {
        let p = if prompt {
            permissions::request()
        } else {
            permissions::check()
        };
        let state = |b: bool| if b { "granted" } else { "MISSING" };
        let mut out = format!(
            "accessibility:    {}\ninput monitoring: {}\npost events:      {}\nsecure input:     {}\n",
            state(p.accessibility),
            state(p.listen_events),
            state(p.post_events),
            if p.secure_input {
                "ON (keystrokes are hidden from macwin until it ends)"
            } else {
                "off"
            },
        );
        if !p.sufficient() {
            out.push_str(
                "\nGrant the missing permissions to the app that runs macwin (MacWin.app, or your terminal) in\n\
                 System Settings > Privacy & Security > Accessibility / Input Monitoring, then restart it.\n",
            );
        }
        out
    }
}

#[cfg(windows)]
mod imp {
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Arc;

    use macwin_platform::tray::TrayApp;
    use macwin_platform::{
        ClipboardBackend, InputCapture, InputInjector, PlatformError, ScreenSource, SecretStore,
    };
    use macwin_platform_windows::{
        DpapiStore, WinCapture, WinClipboard, WinInjector, WinScreens, init_dpi_awareness, tray,
    };
    use macwin_ui::{Backend, Permission, RunOptions, WindowControl};
    use tracing::warn;

    use crate::identity_store::KeyStoreKind;
    use crate::tray::{PermissionState, TrayShared};

    /// The app window on this (main) thread and the notification area icon
    /// on its own thread, until the app has finished. Without a window (no
    /// OpenGL driver) the icon goes on with the classic menu.
    pub fn run_window(shared: Arc<TrayShared>, backend: Arc<dyn Backend>, background: bool) {
        let tray_app: Arc<dyn TrayApp> = shared.clone();
        let tray_thread = std::thread::Builder::new()
            .name("tray".into())
            .spawn(move || tray::run(tray_app));
        let opener = shared.clone();
        let options = RunOptions {
            start_hidden: background,
            hide_on_close: true,
            on_start: Some(Box::new(move |window: WindowControl| {
                opener.set_opener(Box::new(move || window.show()));
            })),
            on_visibility: None,
        };
        let window = macwin_ui::run(backend, options);
        match tray_thread {
            Ok(thread) => {
                if let Err(e) = window {
                    warn!(error = %e, "cannot open the window; the tray icon goes on alone");
                    shared.set_window(false);
                }
                match thread.join() {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => warn!(error = %e, "the tray icon failed"),
                    Err(_) => warn!("the tray icon thread panicked"),
                }
            }
            Err(e) => {
                warn!(error = %e, "cannot start the tray icon thread");
                if let Err(e) = window {
                    warn!(error = %e, "cannot open the window either");
                    shared.quit();
                }
            }
        }
    }

    /// Asks a running MacWin to show its window.
    pub fn show_running_instance() -> bool {
        tray::signal_running_instance()
    }

    pub fn is_builtin(_display: u32) -> bool {
        false
    }

    pub fn permissions() -> Option<PermissionState> {
        None
    }

    pub fn display_names() -> HashMap<u32, String> {
        HashMap::new()
    }

    pub fn request_permissions() {}

    pub fn open_permission_settings(_p: Permission) {}

    /// Per-monitor-v2 DPI awareness; must run before any backend is used.
    pub fn init() {
        let _ = init_dpi_awareness();
    }

    /// The notification area icon, until the app has finished.
    pub fn run_tray(app: Arc<dyn TrayApp>) -> Result<(), PlatformError> {
        tray::run(app)
    }

    pub fn alert(title: &str, message: &str) {
        tray::alert(title, message);
    }

    pub fn screens() -> Box<dyn ScreenSource> {
        Box::new(WinScreens)
    }

    pub fn capture() -> Box<dyn InputCapture> {
        Box::new(WinCapture::new())
    }

    pub fn injector() -> Result<Box<dyn InputInjector>, PlatformError> {
        Ok(Box::new(WinInjector::new()))
    }

    pub fn clipboard() -> Box<dyn ClipboardBackend> {
        Box::new(WinClipboard)
    }

    pub fn device_name() -> Option<String> {
        None
    }

    pub fn os_secret_store(
        kind: KeyStoreKind,
        config_dir: &Path,
    ) -> Result<Box<dyn SecretStore>, PlatformError> {
        match kind {
            KeyStoreKind::Dpapi => Ok(Box::new(DpapiStore::new(config_dir))),
            _ => Err(PlatformError::Unsupported),
        }
    }

    pub fn permissions_report(_prompt: bool) -> String {
        "Windows needs no permission prompts.\n\
         Limits: elevated (admin) windows do not accept input from a non-elevated macwin, and the lock\n\
         screen / UAC prompts are unreachable until the SYSTEM service exists (Phase 4).\n"
            .into()
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod imp {
    use std::path::Path;
    use std::sync::Arc;

    use macwin_platform::tray::TrayApp;
    use macwin_platform::{
        ClipboardBackend, InputCapture, InputInjector, PlatformError, ScreenSource, SecretStore,
        UnsupportedPlatform,
    };

    use crate::identity_store::KeyStoreKind;
    use crate::tray::{PermissionState, TrayShared};

    pub fn init() {}

    pub fn run_tray(_app: Arc<dyn TrayApp>) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported)
    }

    /// Development builds only: the app needs macOS or Windows.
    pub fn run_window(
        shared: Arc<TrayShared>,
        _backend: Arc<dyn macwin_ui::Backend>,
        _background: bool,
    ) {
        shared.quit();
    }

    pub fn show_running_instance() -> bool {
        false
    }

    pub fn is_builtin(_display: u32) -> bool {
        false
    }

    pub fn permissions() -> Option<PermissionState> {
        None
    }

    pub fn display_names() -> std::collections::HashMap<u32, String> {
        std::collections::HashMap::new()
    }

    pub fn request_permissions() {}

    pub fn open_permission_settings(_p: macwin_ui::Permission) {}

    pub fn alert(title: &str, message: &str) {
        eprintln!("{title}: {message}");
    }

    pub fn screens() -> Box<dyn ScreenSource> {
        Box::new(UnsupportedPlatform)
    }

    pub fn capture() -> Box<dyn InputCapture> {
        Box::new(UnsupportedPlatform)
    }

    pub fn injector() -> Result<Box<dyn InputInjector>, PlatformError> {
        Err(PlatformError::Unsupported)
    }

    pub fn clipboard() -> Box<dyn ClipboardBackend> {
        Box::new(UnsupportedPlatform)
    }

    pub fn device_name() -> Option<String> {
        None
    }

    pub fn os_secret_store(
        _kind: KeyStoreKind,
        _config_dir: &Path,
    ) -> Result<Box<dyn SecretStore>, PlatformError> {
        Err(PlatformError::Unsupported)
    }

    pub fn permissions_report(_prompt: bool) -> String {
        "unsupported OS\n".into()
    }
}
