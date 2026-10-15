//! The menu bar item: an `NSStatusItem` with a menu, alerts for notices and
//! a one-line text prompt, all on the main thread. A repeating `NSTimer` in
//! the common run loop modes (so it also fires while the menu is open)
//! polls the agent's view.

use std::cell::{Cell, RefCell};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use macwin_platform::PlatformError;
use macwin_platform::tray::{MenuEntry, TRAY_POLL, TextRequest, TrayApp, TrayIcon, TrayView};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSApplicationActivationPolicy,
    NSControlStateValueOff, NSControlStateValueOn, NSImage, NSMenu, NSMenuItem, NSStatusBar,
    NSStatusItem, NSTextField, NSVariableStatusItemLength, NSView,
};
use objc2_foundation::{
    NSPoint, NSRect, NSRunLoop, NSRunLoopCommonModes, NSSize, NSString, NSTimer, ns_string,
};
use tracing::error;

/// The modal session the tray is in, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Modal {
    Notice,
    Prompt,
}

struct Ivars {
    app: Arc<dyn TrayApp>,
    item: Retained<NSStatusItem>,
    menu: Retained<NSMenu>,
    /// The view version on screen.
    drawn: Cell<Option<u64>>,
    view: RefCell<Option<TrayView>>,
    icon: Cell<Option<TrayIcon>>,
    /// The id of the last notice shown.
    shown: Cell<Option<u64>>,
    modal: Cell<Option<Modal>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and `Target` does not
    // implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    struct Target;

    impl Target {
        #[unsafe(method(tick:))]
        fn tick(&self, _timer: &NSTimer) {
            guarded("timer", || self.on_tick());
        }

        #[unsafe(method(clicked:))]
        fn clicked(&self, sender: &NSMenuItem) {
            let id = u32::try_from(sender.tag()).unwrap_or(0);
            guarded("menu", || self.on_click(id));
        }
    }
);

/// Rust panics must not unwind into AppKit.
fn guarded(what: &str, f: impl FnOnce()) {
    if catch_unwind(AssertUnwindSafe(f)).is_err() {
        error!("the tray {what} handler panicked");
    }
}

fn menu_item(mtm: MainThreadMarker, title: &str, action: Option<Sel>) -> Retained<NSMenuItem> {
    // SAFETY: `action` is a selector `Target` implements, or none.
    unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            action,
            ns_string!(""),
        )
    }
}

/// SF Symbols names (macOS 11+) and accessibility descriptions.
fn symbol(icon: TrayIcon) -> (&'static str, &'static str) {
    match icon {
        TrayIcon::Active => ("display.2", "macwin: connected"),
        TrayIcon::Waiting => ("display", "macwin: waiting"),
        TrayIcon::Paused => ("pause.circle", "macwin: paused"),
        TrayIcon::Problem => ("exclamationmark.triangle", "macwin: needs attention"),
    }
}

/// Brings the (menu bar only) app to the front, so its alerts get focus.
fn activate(mtm: MainThreadMarker) {
    NSApplication::sharedApplication(mtm).activate();
}

fn run_alert(mtm: MainThreadMarker, title: &str, message: &str) {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(title));
    alert.setInformativeText(&NSString::from_str(message));
    alert.addButtonWithTitle(ns_string!("OK"));
    activate(mtm);
    alert.runModal();
}

/// Asks for one line of text; `None` when cancelled.
fn run_prompt(mtm: MainThreadMarker, request: &TextRequest) -> Option<String> {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(&request.title));
    alert.setInformativeText(&NSString::from_str(&request.message));
    alert.addButtonWithTitle(&NSString::from_str(&request.confirm));
    alert.addButtonWithTitle(ns_string!("Cancel"));
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(220.0, 24.0));
    let field = NSTextField::initWithFrame(NSTextField::alloc(mtm), frame);
    field.setPlaceholderString(Some(ns_string!("123-456")));
    let view: &NSView = &field;
    alert.setAccessoryView(Some(view));
    alert.window().setInitialFirstResponder(Some(view));
    activate(mtm);
    (alert.runModal() == NSAlertFirstButtonReturn).then(|| field.stringValue().to_string())
}

impl Target {
    fn new(mtm: MainThreadMarker, app: Arc<dyn TrayApp>) -> Retained<Target> {
        let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        let menu = NSMenu::new(mtm);
        // Items are enabled or not as the view says, not by AppKit.
        menu.setAutoenablesItems(false);
        item.setMenu(Some(&menu));
        let this = Target::alloc(mtm).set_ivars(Ivars {
            app,
            item,
            menu,
            drawn: Cell::new(None),
            view: RefCell::new(None),
            icon: Cell::new(None),
            shown: Cell::new(None),
            modal: Cell::new(None),
        });
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }

    fn on_tick(&self) {
        let iv = self.ivars();
        if iv.app.finished() {
            // Sharing has stopped; this ends the process.
            NSApplication::sharedApplication(self.mtm()).terminate(None);
            return;
        }
        let version = iv.app.version();
        if iv.drawn.get() != Some(version) {
            iv.drawn.set(Some(version));
            let view = iv.app.view();
            self.draw(&view);
            *iv.view.borrow_mut() = Some(view);
        }
        if iv.modal.get().is_none() {
            let notice = iv.view.borrow().as_ref().and_then(|v| v.notice.clone());
            if let Some(n) = notice
                && iv.shown.get() != Some(n.id)
            {
                iv.shown.set(Some(n.id));
                iv.modal.set(Some(Modal::Notice));
                run_alert(self.mtm(), &n.title, &n.message);
                iv.modal.set(None);
            }
        }
    }

    fn draw(&self, view: &TrayView) {
        let iv = self.ivars();
        let mtm = self.mtm();
        if let Some(button) = iv.item.button(mtm) {
            if iv.icon.get() != Some(view.icon) {
                iv.icon.set(Some(view.icon));
                let (name, description) = symbol(view.icon);
                let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
                    &NSString::from_str(name),
                    Some(&NSString::from_str(description)),
                );
                match image {
                    Some(image) => {
                        image.setTemplate(true);
                        button.setImage(Some(&image));
                        button.setTitle(ns_string!(""));
                    }
                    None => {
                        button.setImage(None);
                        button.setTitle(ns_string!("MW"));
                    }
                }
            }
            button.setToolTip(Some(&NSString::from_str(&view.tooltip)));
        }
        iv.menu.removeAllItems();
        let target: &AnyObject = self;
        for entry in &view.menu {
            let item = match entry {
                MenuEntry::Separator => NSMenuItem::separatorItem(mtm),
                MenuEntry::Label(text) => {
                    let item = menu_item(mtm, text, None);
                    item.setEnabled(false);
                    item
                }
                MenuEntry::Item(m) => {
                    let item = menu_item(mtm, &m.title, Some(sel!(clicked:)));
                    // SAFETY: the target lives as long as the menu (until the
                    // process exits).
                    unsafe { item.setTarget(Some(target)) };
                    item.setTag(m.id as isize);
                    item.setEnabled(m.enabled);
                    item.setState(if m.checked {
                        NSControlStateValueOn
                    } else {
                        NSControlStateValueOff
                    });
                    item
                }
            };
            iv.menu.addItem(&item);
        }
    }

    fn on_click(&self, id: u32) {
        let iv = self.ivars();
        let input = iv.view.borrow().as_ref().and_then(|v| {
            v.menu.iter().find_map(|e| match e {
                MenuEntry::Item(m) if m.id == id => Some(m.input.clone()),
                _ => None,
            })
        });
        match input {
            // Not in the current view (it changed meanwhile): ignore.
            None => {}
            Some(None) => iv.app.clicked(id),
            Some(Some(request)) => {
                if iv.modal.get().is_some() {
                    return;
                }
                iv.modal.set(Some(Modal::Prompt));
                let text = run_prompt(self.mtm(), &request);
                iv.modal.set(None);
                if let Some(text) = text {
                    iv.app.entered(id, text);
                }
            }
        }
    }
}

/// Installs the menu bar item into the application on the main thread,
/// whose run loop someone else runs (the app window's event loop). It lives
/// until the process exits, which it ends once the app has finished.
pub fn install(app: Arc<dyn TrayApp>) -> Result<(), PlatformError> {
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| PlatformError::Backend("the tray must run on the main thread".into()))?;
    let target = Target::new(mtm, app);
    target.on_tick();
    let receiver: &AnyObject = &target;
    // SAFETY: `tick:` is implemented by `Target`, which the timer retains
    // (and the run loop retains the timer); the common modes constant is a
    // valid run loop mode.
    unsafe {
        let timer = NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(
            TRAY_POLL.as_secs_f64(),
            receiver,
            sel!(tick:),
            None,
            true,
        );
        NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes);
    }
    Ok(())
}

/// Runs the menu bar item on the main thread until the app has finished;
/// then the process exits.
pub fn run(app: Arc<dyn TrayApp>) -> Result<(), PlatformError> {
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| PlatformError::Backend("the tray must run on the main thread".into()))?;
    let ns_app = NSApplication::sharedApplication(mtm);
    // A menu bar app without a Dock icon, also outside MacWin.app.
    ns_app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    install(app)?;
    ns_app.run();
    Ok(())
}

/// A modal alert outside the tray (startup errors). Main thread only.
pub fn alert(title: &str, message: &str) {
    if let Some(mtm) = MainThreadMarker::new() {
        NSApplication::sharedApplication(mtm)
            .setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        run_alert(mtm, title, message);
    }
}
