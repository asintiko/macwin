//! The notification area icon: a hidden window owns a `Shell_NotifyIconW`
//! icon, shows the menu with `TrackPopupMenu` and the pairing-code prompt as
//! a dialog built in memory (so the dialog manager handles Tab, Enter, Esc
//! and DPI scaling); notices become notification balloons. A timer polls the
//! agent's view. Everything runs on the calling thread. A left click opens
//! the app's window when it has one; another launch asks for it with a
//! registered message ([`signal_running_instance`]).

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::Arc;

use macwin_platform::PlatformError;
use macwin_platform::tray::{MenuEntry, TRAY_POLL, TextRequest, TrayApp, TrayIcon, TrayView};
use tracing::{error, warn};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
    DeleteObject, GetDC, ReleaseDC,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, AppendMenuW, CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon,
    DestroyMenu, DestroyWindow, DialogBoxIndirectParamW, DispatchMessageW, EndDialog, FindWindowW,
    GetCursorPos, GetDlgItem, GetDlgItemTextW, GetMessageW, GetWindowThreadProcessId, HICON, ICONINFO, IDCANCEL, IDI_APPLICATION, IDOK,
    LoadIconW, MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND, MF_CHECKED, MF_GRAYED, MF_SEPARATOR,
    MF_STRING, MSG, MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassExW,
    RegisterWindowMessageW, SM_CXSMICON, SendDlgItemMessageW, SetForegroundWindow, SetTimer,
    TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WM_APP,
    WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY, WM_INITDIALOG, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP,
    WM_TIMER, WNDCLASSEXW, WS_BORDER, WS_CAPTION, WS_CHILD, WS_OVERLAPPED, WS_POPUP, WS_SYSMENU,
    WS_TABSTOP, WS_VISIBLE,
};

/// The icon's callback message.
const WM_TRAY: u32 = WM_APP + 1;
/// The tray window's class: another launch finds the running one by it.
const CLASS: &str = "MacwinTray";
/// The registered message that asks the running instance for its window.
const OPEN_MESSAGE: &str = "MacWin.OpenWindow";
const TIMER_ID: usize = 1;
const ICON_ID: u32 = 1;

// Dialog template styles (winuser.h); not all are in windows-sys features.
const DS_SETFONT: u32 = 0x40;
const DS_MODALFRAME: u32 = 0x80;
const DS_SETFOREGROUND: u32 = 0x200;
const DS_CENTER: u32 = 0x800;
const ES_AUTOHSCROLL: u32 = 0x80;
const BS_PUSHBUTTON: u32 = 0;
const BS_DEFPUSHBUTTON: u32 = 1;
const EM_LIMITTEXT: u32 = 0xC5;
const CLASS_BUTTON: u16 = 0x80;
const CLASS_EDIT: u16 = 0x81;
const CLASS_STATIC: u16 = 0x82;
const EDIT_ID: i32 = 100;

struct Tray {
    app: Arc<dyn TrayApp>,
    hwnd: HWND,
    /// Indexed like [`icon_index`].
    icons: [HICON; 4],
    added: bool,
    drawn: Option<u64>,
    view: Option<TrayView>,
    /// The id of the last notice shown.
    shown: Option<u64>,
}

thread_local! {
    static TRAY: RefCell<Option<Tray>> = const { RefCell::new(None) };
    static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
    static OPEN_REQUEST: Cell<u32> = const { Cell::new(0) };
    static PROMPT_TEXT: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Copies `s` into a fixed NUL-terminated buffer, cut to fit.
fn fill(buf: &mut [u16], s: &str) {
    let max = buf.len() - 1;
    let mut n = 0;
    for (slot, unit) in buf.iter_mut().zip(s.encode_utf16().take(max)) {
        *slot = unit;
        n += 1;
    }
    buf[n] = 0;
}

/// `&` would underline the next letter in a menu.
fn menu_text(s: &str) -> Vec<u16> {
    wide(&s.replace('&', "&&"))
}

fn icon_index(icon: TrayIcon) -> usize {
    match icon {
        TrayIcon::Active => 0,
        TrayIcon::Waiting => 1,
        TrayIcon::Paused => 2,
        TrayIcon::Problem => 3,
    }
}

const ICON_COLORS: [(u8, u8, u8); 4] = [
    (0x2E, 0xA0, 0x43), // connected: green
    (0xF0, 0xAD, 0x4E), // waiting: amber
    (0x8A, 0x8A, 0x8A), // paused: gray
    (0xD9, 0x53, 0x4F), // needs attention: red
];

/// A filled circle of `rgb` with soft edges, `size` pixels square.
fn circle_icon(size: i32, (r, g, b): (u8, u8, u8)) -> HICON {
    // SAFETY: plain GDI calls; `bits` points to size * size pixels owned by
    // the DIB section until it is deleted below (CreateIconIndirect copies).
    unsafe {
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size,
            biHeight: -size, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        };
        let mut bits: *mut c_void = ptr::null_mut();
        let dc = GetDC(ptr::null_mut());
        let color = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, ptr::null_mut(), 0);
        ReleaseDC(ptr::null_mut(), dc);
        if color.is_null() || bits.is_null() {
            return LoadIconW(ptr::null_mut(), IDI_APPLICATION);
        }
        let n = (size * size) as usize;
        let pixels = std::slice::from_raw_parts_mut(bits.cast::<u32>(), n);
        let center = size as f32 / 2.0;
        let radius = center - 1.0;
        for (i, px) in pixels.iter_mut().enumerate() {
            let (x, y) = (
                (i as i32 % size) as f32 + 0.5,
                (i as i32 / size) as f32 + 0.5,
            );
            let d = ((x - center).powi(2) + (y - center).powi(2)).sqrt();
            let alpha = (radius - d + 0.5).clamp(0.0, 1.0);
            let a = (alpha * 255.0) as u32;
            *px = (a << 24) | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);
        }
        // The mask is ignored for icons with alpha, but must exist.
        let mask_bytes = vec![0u8; (size as usize).div_ceil(16) * 2 * size as usize];
        let mask = CreateBitmap(size, size, 1, 1, mask_bytes.as_ptr().cast());
        let icon = CreateIconIndirect(&ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        });
        DeleteObject(color);
        DeleteObject(mask);
        if icon.is_null() {
            LoadIconW(ptr::null_mut(), IDI_APPLICATION)
        } else {
            icon
        }
    }
}

fn notify_data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ICON_ID,
        ..Default::default()
    }
}

impl Tray {
    /// Adds the icon (again, after Explorer restarted) or updates it.
    fn show_icon(&mut self, view: &TrayView) {
        let mut nid = notify_data(self.hwnd);
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        nid.uCallbackMessage = WM_TRAY;
        nid.hIcon = self.icons[icon_index(view.icon)];
        fill(&mut nid.szTip, &view.tooltip);
        // SAFETY: `nid` is a fully initialised NOTIFYICONDATAW for our window.
        unsafe {
            if self.added {
                Shell_NotifyIconW(NIM_MODIFY, &nid);
            } else if Shell_NotifyIconW(NIM_ADD, &nid) != 0 {
                self.added = true;
            }
        }
    }

    fn balloon(&self, title: &str, message: &str) {
        let mut nid = notify_data(self.hwnd);
        nid.uFlags = NIF_INFO;
        nid.dwInfoFlags = NIIF_INFO;
        fill(&mut nid.szInfoTitle, title);
        fill(&mut nid.szInfo, message);
        // SAFETY: as above.
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
    }

    fn remove_icon(&mut self) {
        if self.added {
            let nid = notify_data(self.hwnd);
            // SAFETY: as above.
            unsafe { Shell_NotifyIconW(NIM_DELETE, &nid) };
            self.added = false;
        }
    }

    /// Polls the app: redraws a changed view, shows a new notice. Returns
    /// false once the app has finished.
    fn tick(&mut self) -> bool {
        if self.app.finished() {
            return false;
        }
        let version = self.app.version();
        if self.drawn != Some(version) || !self.added {
            self.drawn = Some(version);
            let view = self.app.view();
            self.show_icon(&view);
            if let Some(n) = &view.notice
                && self.shown != Some(n.id)
                && self.added
            {
                self.shown = Some(n.id);
                self.balloon(&n.title, &n.message);
            }
            self.view = Some(view);
        }
        true
    }
}

/// Runs `f` on the tray state unless it is in use further up the stack
/// (menus and dialogs pump messages, so handlers nest).
fn with_tray<R>(f: impl FnOnce(&mut Tray) -> R) -> Option<R> {
    TRAY.with(|t| {
        let mut guard = t.try_borrow_mut().ok()?;
        guard.as_mut().map(f)
    })
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match catch_unwind(AssertUnwindSafe(|| handle(hwnd, msg, lparam))) {
        Ok(Some(r)) => r,
        Ok(None) => {
            // SAFETY: default handling of everything else.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        Err(_) => {
            error!("the tray window procedure panicked");
            0
        }
    }
}

fn handle(hwnd: HWND, msg: u32, lparam: LPARAM) -> Option<LRESULT> {
    match msg {
        WM_TIMER => {
            if with_tray(Tray::tick) == Some(false) {
                // SAFETY: our own window, on its thread.
                unsafe { DestroyWindow(hwnd) };
            }
            Some(0)
        }
        WM_TRAY => {
            // Without NIM_SETVERSION, lParam is the mouse message.
            match (lparam & 0xFFFF) as u32 {
                WM_LBUTTONUP => {
                    if !open_window() {
                        show_menu(hwnd);
                    }
                }
                WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
                _ => {}
            }
            Some(0)
        }
        m if m != 0 && m == OPEN_REQUEST.get() => {
            open_window();
            Some(0)
        }
        WM_DESTROY => {
            with_tray(Tray::remove_icon);
            // SAFETY: ends this thread's message loop.
            unsafe { PostQuitMessage(0) };
            Some(0)
        }
        m if m != 0 && m == TASKBAR_CREATED.get() => {
            // Explorer restarted and forgot the icon.
            with_tray(|t| {
                t.added = false;
                t.drawn = None;
                t.tick()
            });
            Some(0)
        }
        _ => None,
    }
}

/// Asks the app to show its window; false when it has none.
fn open_window() -> bool {
    // Not under the tray borrow: the app may take a while.
    let app = with_tray(|t| t.app.clone());
    app.is_some_and(|app| app.open())
}

fn show_menu(hwnd: HWND) {
    let Some((view, app)) = with_tray(|t| t.view.clone().map(|v| (v, t.app.clone()))).flatten()
    else {
        return;
    };
    // SAFETY: plain menu calls on a menu we create and destroy; the strings
    // live until each AppendMenuW returns (it copies them).
    let chosen = unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        for entry in &view.menu {
            match entry {
                MenuEntry::Separator => AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null()),
                MenuEntry::Label(text) => {
                    AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, menu_text(text).as_ptr())
                }
                MenuEntry::Item(m) => {
                    let mut flags = MF_STRING;
                    if !m.enabled {
                        flags |= MF_GRAYED;
                    }
                    if m.checked {
                        flags |= MF_CHECKED;
                    }
                    AppendMenuW(menu, flags, m.id as usize, menu_text(&m.title).as_ptr())
                }
            };
        }
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        // Without this the menu stays open when the user clicks elsewhere.
        SetForegroundWindow(hwnd);
        let chosen = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            0,
            hwnd,
            ptr::null(),
        );
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(menu);
        chosen
    };
    let Ok(id) = u32::try_from(chosen) else {
        return;
    };
    let item = view.menu.iter().find_map(|e| match e {
        MenuEntry::Item(m) if m.id == id && id != 0 => Some(m),
        _ => None,
    });
    match item.map(|m| &m.input) {
        None => {}
        Some(None) => app.clicked(id),
        Some(Some(request)) => {
            if let Some(text) = prompt(hwnd, request) {
                app.entered(id, text);
            }
        }
    }
}

/// An in-memory `DLGTEMPLATE`, built as UTF-16 words.
struct Template(Vec<u16>);

impl Template {
    fn word(&mut self, w: u16) {
        self.0.push(w);
    }

    fn dword(&mut self, d: u32) {
        self.0.push(d as u16);
        self.0.push((d >> 16) as u16);
    }

    fn text(&mut self, s: &str) {
        self.0.extend(s.encode_utf16());
        self.0.push(0);
    }

    /// Items start on a DWORD boundary.
    fn align(&mut self) {
        if self.0.len() % 2 == 1 {
            self.0.push(0);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn item(&mut self, style: u32, x: u16, y: u16, cx: u16, cy: u16, id: u16, class: u16, s: &str) {
        self.align();
        self.dword(WS_CHILD | WS_VISIBLE | style);
        self.dword(0);
        for v in [x, y, cx, cy, id] {
            self.word(v);
        }
        self.word(0xFFFF);
        self.word(class);
        self.text(s);
        self.word(0); // no creation data
    }

    /// The words copied into DWORD-aligned memory.
    fn aligned(&self) -> Vec<u32> {
        self.0
            .chunks(2)
            .map(|c| u32::from(c[0]) | (u32::from(c.get(1).copied().unwrap_or(0)) << 16))
            .collect()
    }
}

/// Message, edit field, confirm and Cancel (dialog units).
fn prompt_template(request: &TextRequest) -> Vec<u32> {
    let mut t = Template(Vec::new());
    t.dword(
        WS_POPUP
            | WS_CAPTION
            | WS_SYSMENU
            | DS_SETFONT
            | DS_MODALFRAME
            | DS_CENTER
            | DS_SETFOREGROUND,
    );
    t.dword(0);
    t.word(4); // items
    for v in [0, 0, 230, 82] {
        t.word(v);
    }
    t.word(0); // no menu
    t.word(0); // default class
    t.text(&request.title);
    t.word(9);
    t.text("Segoe UI");
    t.item(0, 7, 7, 216, 30, 0xFFFF, CLASS_STATIC, &request.message);
    let edit = (WS_BORDER | WS_TABSTOP) | ES_AUTOHSCROLL;
    t.item(edit, 7, 40, 216, 14, EDIT_ID as u16, CLASS_EDIT, "");
    let ok = WS_TABSTOP | BS_DEFPUSHBUTTON;
    t.item(
        ok,
        119,
        61,
        50,
        14,
        IDOK as u16,
        CLASS_BUTTON,
        &request.confirm,
    );
    let cancel = WS_TABSTOP | BS_PUSHBUTTON;
    t.item(
        cancel,
        173,
        61,
        50,
        14,
        IDCANCEL as u16,
        CLASS_BUTTON,
        "Cancel",
    );
    t.aligned()
}

unsafe extern "system" fn prompt_proc(dlg: HWND, msg: u32, wparam: WPARAM, _: LPARAM) -> isize {
    let handled = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: dialog calls on the dialog Windows passed us.
        unsafe {
            match msg {
                WM_INITDIALOG => {
                    SendDlgItemMessageW(dlg, EDIT_ID, EM_LIMITTEXT, 32, 0);
                    SetFocus(GetDlgItem(dlg, EDIT_ID));
                    // FALSE: focus was set here.
                    0
                }
                WM_COMMAND => match (wparam & 0xFFFF) as i32 {
                    IDOK => {
                        let mut buf = [0u16; 64];
                        let n = GetDlgItemTextW(dlg, EDIT_ID, buf.as_mut_ptr(), buf.len() as i32);
                        let text = String::from_utf16_lossy(&buf[..n as usize]);
                        PROMPT_TEXT.with(|p| *p.borrow_mut() = Some(text));
                        EndDialog(dlg, 1);
                        1
                    }
                    IDCANCEL => {
                        EndDialog(dlg, 0);
                        1
                    }
                    _ => 0,
                },
                _ => 0,
            }
        }
    }));
    handled.unwrap_or(0)
}

/// Asks for one line of text; `None` when cancelled.
fn prompt(owner: HWND, request: &TextRequest) -> Option<String> {
    let template = prompt_template(request);
    PROMPT_TEXT.with(|p| p.borrow_mut().take());
    // SAFETY: the template is a valid, DWORD-aligned DLGTEMPLATE that lives
    // until the modal dialog returns.
    let r = unsafe {
        DialogBoxIndirectParamW(
            GetModuleHandleW(ptr::null()),
            template.as_ptr().cast(),
            owner,
            Some(prompt_proc),
            0,
        )
    };
    if r == -1 {
        warn!("cannot show the pairing code prompt");
    }
    if r == 1 {
        PROMPT_TEXT.with(|p| p.borrow_mut().take())
    } else {
        None
    }
}

/// Runs the notification area icon on the calling thread until the app has
/// finished.
pub fn run(app: Arc<dyn TrayApp>) -> Result<(), PlatformError> {
    let class = wide(CLASS);
    let title = wide("macwin");
    // SAFETY: plain Win32 window and message loop calls on this thread; the
    // class and title buffers outlive the calls that use them.
    unsafe {
        let hinstance = GetModuleHandleW(ptr::null());
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(window_proc),
            hInstance: hinstance,
            lpszClassName: class.as_ptr(),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        // A hidden top-level window: message-only windows miss the
        // "TaskbarCreated" broadcast.
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            hinstance,
            ptr::null(),
        );
        if hwnd.is_null() {
            return Err(PlatformError::Backend(
                "cannot create the tray window".into(),
            ));
        }
        TASKBAR_CREATED.set(RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()));
        OPEN_REQUEST.set(RegisterWindowMessageW(wide(OPEN_MESSAGE).as_ptr()));
        let size = GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForSystem()).max(16);
        let icons = ICON_COLORS.map(|c| circle_icon(size, c));
        TRAY.with(|t| {
            *t.borrow_mut() = Some(Tray {
                app,
                hwnd,
                icons,
                added: false,
                drawn: None,
                view: None,
                shown: None,
            })
        });
        with_tray(Tray::tick);
        SetTimer(hwnd, TIMER_ID, TRAY_POLL.as_millis() as u32, None);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        if let Some(tray) = TRAY.with(|t| t.borrow_mut().take()) {
            for icon in tray.icons {
                DestroyIcon(icon);
            }
        }
    }
    Ok(())
}

/// Asks an already running instance to show its window and lets it come to
/// the foreground. True when one was found.
pub fn signal_running_instance() -> bool {
    let class = wide(CLASS);
    let message = wide(OPEN_MESSAGE);
    // SAFETY: plain Win32 calls with NUL-terminated strings that outlive
    // them; posting to another process's window is allowed for registered
    // messages.
    unsafe {
        let hwnd = FindWindowW(class.as_ptr(), ptr::null());
        if hwnd.is_null() {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid != 0 {
            AllowSetForegroundWindow(pid);
        }
        PostMessageW(hwnd, RegisterWindowMessageW(message.as_ptr()), 0, 0) != 0
    }
}

/// A message box outside the tray (startup errors).
pub fn alert(title: &str, message: &str) {
    let (title, message) = (wide(title), wide(message));
    // SAFETY: both strings are NUL-terminated and outlive the call.
    unsafe {
        MessageBoxW(
            ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND,
        )
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_buffers_are_cut_and_terminated() {
        let mut buf = [1u16; 4];
        fill(&mut buf, "abcdef");
        assert_eq!(buf, [97, 98, 99, 0]);
        fill(&mut buf, "a");
        assert_eq!(&buf[..2], &[97, 0]);
    }

    #[test]
    fn the_prompt_template_is_aligned_and_complete() {
        let request = TextRequest {
            title: "Enter pairing code".into(),
            message: "Type the code".into(),
            confirm: "Pair".into(),
        };
        let words: Vec<u16> = prompt_template(&request)
            .iter()
            .flat_map(|d| [*d as u16, (*d >> 16) as u16])
            .collect();
        assert_eq!(words[4], 4, "four items");
        assert_eq!(&words[7..9], &[230, 82]);
        // Every item header starts on a DWORD boundary: find the class
        // markers (0xFFFF, atom) and check the header 9 words before.
        let mut starts = Vec::new();
        for i in 9..words.len() - 1 {
            if words[i] == 0xFFFF
                && matches!(words[i + 1], CLASS_BUTTON | CLASS_EDIT | CLASS_STATIC)
            {
                starts.push(i - 9);
            }
        }
        assert_eq!(starts.len(), 4, "{starts:?}");
        assert!(starts.iter().all(|s| s % 2 == 0), "{starts:?}");
        assert_eq!(menu_text("A&B"), wide("A&&B"));
    }
}
