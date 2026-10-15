//! The eframe application: it mirrors the backend state, shows the wizard or
//! the main window, and manages the window's visibility.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Layout, TextureHandle, ThemePreference, ViewportCommand, vec2};
use macwin_core::arrange::Placement;

use crate::editor::EditorState;
use crate::flow::Flow;
use crate::i18n::Tr;
use crate::model::{Backend, Status, UiCommand, UiState};
use crate::theme::{self, Palette};
use crate::{home, icon, widgets, wizard};

/// How long closing the window waits for the app to shut down.
const QUIT_WAIT: Duration = Duration::from_secs(5);

/// How the window starts and behaves.
#[derive(Default)]
pub struct RunOptions {
    /// Autostart at login: no window until asked for.
    pub start_hidden: bool,
    /// Closing the window hides it and the app keeps running in the tray.
    pub hide_on_close: bool,
    /// Called once on the UI thread when the event loop runs; the app
    /// installs the macOS status item here.
    pub on_start: Option<Box<dyn FnOnce(WindowControl)>>,
    /// Called on the UI thread when the window is shown (`true`) or hidden
    /// (`false`); the app toggles the macOS Dock icon here.
    pub on_visibility: Option<Box<dyn Fn(bool)>>,
}

/// A handle that works from any thread.
#[derive(Clone)]
pub struct WindowControl {
    ctx: egui::Context,
    show: Arc<AtomicBool>,
}

impl WindowControl {
    /// Shows the window, brings it to the front and focuses it.
    pub fn show(&self) {
        self.show.store(true, Ordering::SeqCst);
        self.ctx.request_repaint();
    }

    /// Wakes the UI thread so it picks up a state change.
    pub fn request_repaint(&self) {
        self.ctx.request_repaint();
    }
}

/// The tabs of the main window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tab {
    Arrangement,
    Clipboard,
    Settings,
    About,
}

/// What the views share while they draw: the state mirror, the way back to
/// the application, and the theme.
pub(crate) struct Cx<'a> {
    pub st: &'a mut UiState,
    pub backend: &'a dyn Backend,
    pub pal: Palette,
    pub tr: Tr,
}

impl Cx<'_> {
    pub fn send(&self, cmd: UiCommand) {
        self.backend.send(cmd);
    }
}

/// State of the views that lives between frames.
pub(crate) struct Local {
    pub flow: Flow,
    pub tab: Tab,
    pub editor: EditorState,
    /// A placement just sent, shown until the backend reports it.
    pub pending: Option<(Placement, Instant)>,
    pub code: String,
    pub address: String,
    pub address_open: bool,
    pub confirm_unpair: bool,
    pub dismissed: Option<u64>,
    pub logo: Option<TextureHandle>,
}

pub(crate) struct MacWinApp {
    backend: Arc<dyn Backend>,
    st: UiState,
    seen: u64,
    local: Local,
    hide_on_close: bool,
    start_hidden: bool,
    on_visibility: Option<Box<dyn Fn(bool)>>,
    show_requested: Arc<AtomicBool>,
    shown: bool,
    started: bool,
    closing: bool,
}

impl MacWinApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        backend: Arc<dyn Backend>,
        options: RunOptions,
    ) -> MacWinApp {
        let ctx = cc.egui_ctx.clone();
        theme::install(&ctx);
        match std::env::var("MACWIN_UI_THEME").as_deref() {
            Ok("dark") => ctx.set_theme(ThemePreference::Dark),
            Ok("light") => ctx.set_theme(ThemePreference::Light),
            _ => {}
        }
        let waker_ctx = ctx.clone();
        backend.set_waker(Box::new(move || waker_ctx.request_repaint()));
        let seen = backend.version();
        let st = backend.state();
        let show_requested = Arc::new(AtomicBool::new(false));
        if let Some(on_start) = options.on_start {
            on_start(WindowControl {
                ctx: ctx.clone(),
                show: Arc::clone(&show_requested),
            });
        }
        MacWinApp {
            backend,
            st,
            seen,
            local: Local {
                flow: Flow::new(),
                tab: Tab::Arrangement,
                editor: EditorState::default(),
                pending: None,
                code: String::new(),
                address: String::new(),
                address_open: false,
                confirm_unpair: false,
                dismissed: None,
                logo: icon::logo_texture(&ctx),
            },
            hide_on_close: options.hide_on_close,
            start_hidden: options.start_hidden,
            on_visibility: options.on_visibility,
            show_requested,
            shown: true,
            started: false,
            closing: false,
        }
    }

    fn set_shown(&mut self, shown: bool) {
        if self.started && self.shown == shown {
            return;
        }
        self.shown = shown;
        if let Some(f) = &self.on_visibility {
            f(shown);
        }
    }
}

impl eframe::App for MacWinApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.started {
            if self.start_hidden {
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
            }
            self.set_shown(!self.start_hidden);
            self.started = true;
        }

        let version = self.backend.version();
        if version != self.seen {
            self.st = self.backend.state();
            self.seen = version;
        }

        if self.backend.finished() && !self.closing {
            self.closing = true;
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }

        if self.show_requested.swap(false, Ordering::SeqCst) && !self.closing {
            ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(ViewportCommand::Focus);
            ctx.request_repaint();
            self.set_shown(true);
        }

        if ctx.input(|i| i.viewport().close_requested()) && self.hide_on_close && !self.closing {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
            self.set_shown(false);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let pal = Palette::of(ui);
        let tr = Tr(self.st.lang);
        for cmd in self.local.flow.sync(&self.st) {
            self.backend.send(cmd);
        }
        let mut cx = Cx {
            st: &mut self.st,
            backend: &*self.backend,
            pal,
            tr,
        };
        let waiting = matches!(cx.st.status, Status::Starting) && cx.st.peer.is_none();
        if matches!(cx.st.status, Status::Quitting) {
            splash(ui, &cx, cx.tr.quitting());
        } else if self.local.flow.active() {
            wizard::show(ui, &mut cx, &mut self.local);
        } else if waiting {
            splash(ui, &cx, cx.tr.starting());
        } else {
            home::show(ui, &mut cx, &mut self.local);
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.backend.finished() {
            return;
        }
        self.backend.send(UiCommand::Quit);
        let deadline = Instant::now() + QUIT_WAIT;
        while !self.backend.finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.panel_fill.to_normalized_gamma_f32()
    }
}

/// The state's notice as a banner unless the user has dismissed it; true
/// when one was shown.
pub(crate) fn notice(ui: &mut egui::Ui, cx: &Cx<'_>, l: &mut Local) -> bool {
    let Some(n) = cx.st.notice.clone().filter(|n| l.dismissed != Some(n.id)) else {
        return false;
    };
    if widgets::banner(ui, &cx.pal, n.error, &n.title, &n.message, cx.tr.dismiss()) {
        l.dismissed = Some(n.id);
        cx.send(UiCommand::DismissNotice(n.id));
    }
    true
}

/// A centred spinner with a line of text.
fn splash(ui: &mut egui::Ui, cx: &Cx<'_>, text: &str) {
    egui::CentralPanel::default().show(ui, |ui| {
        ui.with_layout(Layout::top_down(Align::Center), |ui| {
            ui.add_space((ui.available_height() / 2.0 - 40.0).max(0.0));
            ui.add(egui::Spinner::new().size(32.0).color(cx.pal.accent));
            ui.add_space(14.0);
            widgets::paragraph(ui, text, 16.0, cx.pal.muted, Align::Center);
            ui.allocate_space(vec2(0.0, 0.0));
        });
    });
}

/// Starts the application window and returns when it has closed.
///
/// `backend` provides the state and receives the commands. The window runs
/// on the calling thread, which must be the main thread.
pub fn run(backend: Arc<dyn Backend>, options: RunOptions) -> Result<(), eframe::Error> {
    let start_hidden = options.start_hidden;
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("MacWin")
        .with_app_id("macwin")
        .with_inner_size([880.0, 620.0])
        .with_min_inner_size([760.0, 540.0]);
    if let Some(icon) = icon::window_icon() {
        viewport = viewport.with_icon(icon);
    }
    let hook: eframe::EventLoopBuilderHook = Box::new(move |builder| {
        #[cfg(target_os = "macos")]
        crate::platform::configure(builder, start_hidden);
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (builder, start_hidden);
        }
    });
    let native = eframe::NativeOptions {
        viewport,
        event_loop_builder: Some(hook),
        ..Default::default()
    };
    eframe::run_native(
        "MacWin",
        native,
        Box::new(move |cc| Ok(Box::new(MacWinApp::new(cc, backend, options)))),
    )
}
