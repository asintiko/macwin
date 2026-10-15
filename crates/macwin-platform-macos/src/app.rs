//! The application around MacWin's window: the Dock icon, bringing the app
//! to the front, "opened again" from Finder or the Dock, the privacy panes of
//! System Settings and display names. AppKit calls are main-thread only;
//! called elsewhere they do nothing.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSScreen, NSWorkspace};
use objc2_core_graphics::CGDisplayIsBuiltin;
use objc2_foundation::{NSAppleEventManager, NSNumber, NSString, NSURL, ns_string};
use tracing::error;

/// `kCoreEventClass` and `kAEReopenApplication` (four-character codes).
const CORE_EVENT_CLASS: u32 = u32::from_be_bytes(*b"aevt");
const REOPEN_APPLICATION: u32 = u32::from_be_bytes(*b"rapp");

/// Shows the Dock icon (and the app's menu) while the window is open; a menu
/// bar-only app otherwise.
pub fn set_dock_icon(visible: bool) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let policy = if visible {
        NSApplicationActivationPolicy::Regular
    } else {
        NSApplicationActivationPolicy::Accessory
    };
    if app.activationPolicy() != policy {
        app.setActivationPolicy(policy);
    }
}

/// Brings the app to the front.
pub fn activate() {
    if let Some(mtm) = MainThreadMarker::new() {
        NSApplication::sharedApplication(mtm).activate();
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and `ReopenHandler`
    // does not implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Box<dyn Fn()>]
    struct ReopenHandler;

    impl ReopenHandler {
        #[unsafe(method(handleReopen:withReplyEvent:))]
        fn handle_reopen(&self, _event: &AnyObject, _reply: &AnyObject) {
            if catch_unwind(AssertUnwindSafe(|| (self.ivars())())).is_err() {
                error!("the reopen handler panicked");
            }
        }
    }
);

/// Calls `f` whenever the running app is opened again (Finder, the Dock,
/// `open -a`). Call it after the app has finished launching, because AppKit
/// installs its own handler while launching; ours replaces it.
pub fn on_reopen(f: Box<dyn Fn()>) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let handler = ReopenHandler::alloc(mtm).set_ivars(f);
    // SAFETY: NSObject's designated initializer.
    let handler: Retained<ReopenHandler> = unsafe { msg_send![super(handler), init] };
    let manager = NSAppleEventManager::sharedAppleEventManager();
    let target: &AnyObject = &handler;
    // SAFETY: the selector is implemented by the handler with the
    // `(NSAppleEventDescriptor *, NSAppleEventDescriptor *)` signature;
    // AEEventClass and AEEventID are 32-bit four-character codes.
    unsafe {
        let _: () = msg_send![
            &manager,
            setEventHandler: target,
            andSelector: sel!(handleReopen:withReplyEvent:),
            forEventClass: CORE_EVENT_CLASS,
            andEventID: REOPEN_APPLICATION
        ];
    }
    // The event manager does not retain its handlers.
    std::mem::forget(handler);
}

/// A privacy pane of System Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivacyPane {
    Accessibility,
    InputMonitoring,
}

/// Opens `pane` in System Settings; false when it could not be opened.
pub fn open_privacy_settings(pane: PrivacyPane) -> bool {
    let anchor = match pane {
        PrivacyPane::Accessibility => "Privacy_Accessibility",
        PrivacyPane::InputMonitoring => "Privacy_ListenEvent",
    };
    let url = format!("x-apple.systempreferences:com.apple.preference.security?{anchor}");
    match NSURL::URLWithString(&NSString::from_str(&url)) {
        Some(url) => NSWorkspace::sharedWorkspace().openURL(&url),
        None => false,
    }
}

/// The user-visible names of the connected displays ("Built-in Retina
/// Display", "LG UltraFine"), by display id.
pub fn display_names() -> HashMap<u32, String> {
    let Some(mtm) = MainThreadMarker::new() else {
        return HashMap::new();
    };
    let mut names = HashMap::new();
    for screen in NSScreen::screens(mtm) {
        let number = screen
            .deviceDescription()
            .objectForKey(ns_string!("NSScreenNumber"))
            .and_then(|n| n.downcast::<NSNumber>().ok());
        if let Some(number) = number {
            names.insert(
                number.unsignedIntValue(),
                screen.localizedName().to_string(),
            );
        }
    }
    names
}

/// True for a laptop's built-in display. Any thread.
pub fn is_builtin(display: u32) -> bool {
    CGDisplayIsBuiltin(display)
}
