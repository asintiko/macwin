//! macOS specifics of the event loop.

use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS as _};

/// Sets how the app presents itself to macOS before the loop starts.
pub(crate) fn configure(builder: &mut eframe::EventLoopBuilder<eframe::UserEvent>, start_hidden: bool) {
    if start_hidden {
        // Autostart at login: no Dock icon and no focus stealing until the
        // window is asked for.
        builder
            .with_activation_policy(ActivationPolicy::Accessory)
            .with_activate_ignoring_other_apps(false);
    } else {
        // Explicit, or winit honours LSUIElement of the bundle and the window
        // would never get a menu bar or focus.
        builder.with_activation_policy(ActivationPolicy::Regular);
    }
}
