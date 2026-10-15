//! The main window of a paired computer: the status header and the tabs.

use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, CornerRadius, FontId, Frame, Id, Key, Layout, Margin, RichText, Sense, Stroke,
    Ui, pos2, vec2,
};
use macwin_core::arrange::{Edge, Placement};

use crate::app::{Cx, Local, Tab};
use crate::editor::{self, Params};
use crate::groups;
use crate::i18n::Tr;
use crate::model::{Activity, Lang, Permission, Role, Status, UiCommand, UiState};
use crate::theme::{Palette, Tone};
use crate::widgets::{self, Btn, badge, banner, button, card, centered_column, divider, heavy, paragraph, pill, segmented, tab_bar, toggle_row};
use crate::wizard::default_placement;

/// How long a placement just sent is shown before the backend's word wins.
const PENDING_FOR: Duration = Duration::from_secs(2);

/// How a status reads in the header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StatusView {
    pub tone: Tone,
    pub text: String,
    pub detail: Option<String>,
}

/// The header text, colour and detail for the state.
pub(crate) fn status_view(tr: Tr, st: &UiState, crossing: Option<(Edge, &str)>) -> StatusView {
    let peer = st.peer.as_ref().map_or("—", |p| p.name.as_str());
    let (tone, text, detail) = match &st.status {
        Status::Connected(Activity::Idle) => (
            Tone::Good,
            tr.st_connected(peer),
            crossing.map(|(e, a)| tr.st_detail_primary_edge(e, a)),
        ),
        Status::Connected(Activity::Controlling) => (Tone::Good, tr.st_controlling(peer), None),
        Status::Connected(Activity::Controlled) => (Tone::Good, tr.st_controlled(peer), None),
        Status::Listening => (Tone::Wait, tr.st_listening(peer), None),
        Status::Connecting => (Tone::Wait, tr.st_connecting(peer), None),
        Status::Retrying(why) => (Tone::Wait, tr.st_retrying(peer), Some(why.clone())),
        Status::Paused => (
            Tone::Idle,
            tr.st_paused().to_owned(),
            Some(tr.st_paused_detail().to_owned()),
        ),
        Status::EmergencyPaused => (
            Tone::Bad,
            tr.st_emergency().to_owned(),
            Some(tr.st_emergency_detail().to_owned()),
        ),
        Status::Failed(why) => (Tone::Bad, tr.st_failed().to_owned(), Some(why.clone())),
        Status::NeedsArrangement => (
            Tone::Wait,
            tr.st_needs_arrangement().to_owned(),
            Some(tr.st_needs_arrangement_detail().to_owned()),
        ),
        Status::PairingWait { .. } | Status::PairingDial => {
            (Tone::Wait, tr.st_pairing().to_owned(), None)
        }
        Status::NotPaired => (Tone::Wait, tr.st_not_paired().to_owned(), None),
        Status::Starting => (Tone::Wait, tr.starting().to_owned(), None),
        Status::Quitting => (Tone::Idle, tr.quitting().to_owned(), None),
    };
    StatusView { tone, text, detail }
}

fn pausable(status: &Status) -> bool {
    !matches!(status, Status::Starting | Status::Quitting | Status::Failed(_))
}

fn paused(status: &Status) -> bool {
    matches!(status, Status::Paused | Status::EmergencyPaused)
}

/// The tab selected by Cmd/Ctrl + 1..4.
fn shortcut_tab(i: &egui::InputState) -> Option<Tab> {
    if !i.modifiers.command {
        return None;
    }
    [
        (Key::Num1, Tab::Arrangement),
        (Key::Num2, Tab::Clipboard),
        (Key::Num3, Tab::Settings),
        (Key::Num4, Tab::About),
    ]
    .into_iter()
    .find(|(k, _)| i.key_pressed(*k))
    .map(|(_, t)| t)
}

/// The main window.
pub(crate) fn show(ui: &mut Ui, cx: &mut Cx<'_>, l: &mut Local) {
    let pal = cx.pal;
    if let Some(tab) = ui.input(shortcut_tab) {
        l.tab = tab;
    }
    if let Some((p, at)) = l.pending {
        if cx.st.placement == Some(p) || at.elapsed() > PENDING_FOR {
            l.pending = None;
        }
    }
    egui::Panel::top("home_top")
        .frame(Frame::new().fill(pal.bg).inner_margin(Margin {
            left: 28,
            right: 28,
            top: 18,
            bottom: 0,
        }))
        .show_separator_line(false)
        .show(ui, |ui| header(ui, cx, l));
    egui::CentralPanel::default()
        .frame(Frame::new().fill(pal.bg).inner_margin(Margin::symmetric(28, 16)))
        .show(ui, |ui| match l.tab {
            Tab::Arrangement => arrangement_tab(ui, cx, l),
            Tab::Clipboard => scroll(ui, |ui| clipboard_tab(ui, cx)),
            Tab::Settings => scroll(ui, |ui| settings_tab(ui, cx, l)),
            Tab::About => scroll(ui, |ui| about_tab(ui, cx, l)),
        });
    unpair_dialog(ui, cx, l);
}

fn scroll(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| centered_column(ui, 900.0, add));
}

// ---- header ------------------------------------------------------------

fn header(ui: &mut Ui, cx: &mut Cx<'_>, l: &mut Local) {
    let (pal, tr) = (cx.pal, cx.tr);
    centered_column(ui, 900.0, |ui| {
        if let Some(n) = cx.st.notice.clone() {
            if l.dismissed != Some(n.id) {
                if banner(ui, &pal, n.error, &n.title, &n.message, tr.dismiss()) {
                    l.dismissed = Some(n.id);
                    cx.send(UiCommand::DismissNotice(n.id));
                }
                ui.add_space(10.0);
            }
        }
        status_card(ui, cx, l);
        ui.add_space(8.0);
        let items = [
            (Tab::Arrangement, tr.tab_arrangement()),
            (Tab::Clipboard, tr.tab_clipboard()),
            (Tab::Settings, tr.tab_settings()),
            (Tab::About, tr.tab_about()),
        ];
        tab_bar(ui, &pal, &items, &mut l.tab);
        divider(ui, &pal);
    });
}

fn status_card(ui: &mut Ui, cx: &mut Cx<'_>, l: &mut Local) {
    let (pal, tr) = (cx.pal, cx.tr);
    let role = cx.st.role.unwrap_or(Role::Primary);
    let crossing = (role == Role::Primary)
        .then(|| {
            let groups = groups::build(cx.st, role, tr);
            let p = cx.st.placement?;
            let scene = macwin_core::arrange::scene(&groups.anchor.screens, &groups.moving.screens, &p)?;
            let name = if groups.anchor.screens.len() == 1 {
                groups.anchor.name.clone()
            } else {
                editor::anchor_label(&groups, &scene).to_owned()
            };
            Some((p.edge, name))
        })
        .flatten();
    let view = status_view(tr, cx.st, crossing.as_ref().map(|(e, n)| (*e, n.as_str())));
    let _ = l;
    card(ui, &pal, |ui| {
        ui.horizontal(|ui| {
            pill(ui, &pal, view.tone, &view.text);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if pausable(&cx.st.status) {
                    let is_paused = paused(&cx.st.status);
                    let (label, kind) = if is_paused {
                        (tr.resume(), Btn::Primary)
                    } else {
                        (tr.pause(), Btn::Secondary)
                    };
                    if widgets::button_sized(ui, &pal, label, kind, 120.0).clicked() {
                        cx.send(UiCommand::SetPaused(!is_paused));
                    }
                }
                badge(ui, &pal, tr.role_badge(role == Role::Primary));
            });
        });
        if let Some(d) = &view.detail {
            ui.add_space(6.0);
            ui.label(RichText::new(d).size(14.0).color(pal.muted));
        }
    });
}

// ---- arrangement -------------------------------------------------------

/// The placement to show: what was just sent, else the backend's.
fn shown_placement(st: &UiState, l: &Local, role: Role) -> Option<Placement> {
    match role {
        Role::Primary => l
            .pending
            .map(|(p, _)| p)
            .or(st.placement)
            .or_else(|| Some(default_placement(st))),
        Role::Secondary => st.placement,
    }
}

fn arrangement_tab(ui: &mut Ui, cx: &mut Cx<'_>, l: &mut Local) {
    let (pal, tr) = (cx.pal, cx.tr);
    let role = cx.st.role.unwrap_or(Role::Primary);
    let editable = role == Role::Primary;
    centered_column(ui, 900.0, |ui| {
        let top = ui.available_height();
        if editable {
            paragraph(ui, tr.arrange_hint(), 14.5, pal.muted, Align::Min);
        } else {
            lock_note(ui, &pal, tr.read_only_note());
        }
        ui.add_space(12.0);
        let groups = groups::build(cx.st, role, tr);
        let Some(placement) = shown_placement(cx.st, l, role) else {
            ui.add_space(24.0);
            paragraph(ui, tr.no_arrangement_yet(), 15.0, pal.muted, Align::Center);
            return;
        };
        let reserved = if cx.st.placement.is_none() && editable { 150.0 } else { 108.0 };
        let height = (top - reserved).clamp(180.0, 480.0);
        let params = Params {
            groups: &groups,
            placement,
            editable,
            height,
        };
        let dropped = l.editor.show(ui, &pal, tr, &params);
        if let Some(p) = dropped {
            l.pending = Some((p, Instant::now()));
            cx.send(UiCommand::SetPlacement(p));
        }
        ui.add_space(10.0);
        editor::legend(ui, &pal, tr, &groups);
        let shown = l.pending.map_or(placement, |(p, _)| p);
        if let Some(text) = editor::caption(tr, &groups, &shown) {
            ui.add_space(2.0);
            ui.label(RichText::new(text).size(14.0).color(pal.text));
        }
        if editable && cx.st.placement.is_none() {
            ui.add_space(8.0);
            if button(ui, &pal, tr.save_arrangement(), Btn::Primary).clicked() {
                l.pending = Some((placement, Instant::now()));
                cx.send(UiCommand::SetPlacement(placement));
            }
        }
    });
}

/// A quiet note with a bar on its left edge.
fn lock_note(ui: &mut Ui, pal: &Palette, text: &str) {
    Frame::new()
        .fill(pal.card)
        .stroke(Stroke::new(1.0, pal.border))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(text).size(14.5).color(pal.muted));
        });
}

// ---- clipboard ---------------------------------------------------------

fn clipboard_tab(ui: &mut Ui, cx: &mut Cx<'_>) {
    let (pal, tr) = (cx.pal, cx.tr);
    ui.add_space(4.0);
    card(ui, &pal, |ui| {
        let mut s = cx.st.settings;
        let mut changed = false;
        changed |= toggle_row(
            ui,
            &pal,
            tr.clipboard_shared(),
            Some(tr.clipboard_shared_desc()),
            &mut s.clipboard,
            true,
        );
        ui.add_space(12.0);
        divider(ui, &pal);
        ui.add_space(12.0);
        changed |= toggle_row(ui, &pal, tr.clipboard_images(), None, &mut s.images, s.clipboard);
        ui.add_space(12.0);
        divider(ui, &pal);
        ui.add_space(12.0);
        changed |= toggle_row(ui, &pal, tr.clipboard_files(), None, &mut s.files, s.clipboard);
        if changed {
            cx.st.settings = s;
            cx.send(UiCommand::SetClipboard {
                enabled: s.clipboard,
                images: s.images,
                files: s.files,
            });
        }
    });
    ui.add_space(12.0);
    ui.label(RichText::new(tr.clipboard_limits()).size(13.5).color(pal.muted));
}

// ---- settings ----------------------------------------------------------

fn settings_tab(ui: &mut Ui, cx: &mut Cx<'_>, l: &mut Local) {
    let (pal, tr) = (cx.pal, cx.tr);
    ui.add_space(4.0);
    card(ui, &pal, |ui| {
        let mut s = cx.st.settings;
        let mut autostart = s.autostart.unwrap_or(false);
        let desc = s.autostart.is_none().then(|| tr.set_autostart_na());
        if toggle_row(ui, &pal, tr.set_autostart(), desc, &mut autostart, s.autostart.is_some()) {
            s.autostart = Some(autostart);
            cx.st.settings = s;
            cx.send(UiCommand::SetAutostart(autostart));
        }
        ui.add_space(12.0);
        divider(ui, &pal);
        ui.add_space(12.0);
        if toggle_row(ui, &pal, tr.set_remap(), Some(tr.set_remap_desc()), &mut s.remap, true) {
            cx.st.settings = s;
            cx.send(UiCommand::SetRemap(s.remap));
        }
        ui.add_space(12.0);
        divider(ui, &pal);
        ui.add_space(12.0);
        if toggle_row(
            ui,
            &pal,
            tr.set_discovery(),
            Some(tr.set_discovery_desc()),
            &mut s.discovery,
            true,
        ) {
            cx.st.settings = s;
            cx.send(UiCommand::SetDiscovery(s.discovery));
        }
    });
    ui.add_space(14.0);
    card(ui, &pal, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr.set_language()).size(15.5).color(pal.text));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let selected = usize::from(cx.st.lang == Lang::En);
                if let Some(i) = segmented(ui, &pal, &["Русский", "English"], selected) {
                    let lang = if i == 0 { Lang::Ru } else { Lang::En };
                    cx.st.lang = lang;
                    cx.send(UiCommand::SetLanguage(lang));
                }
            });
        });
    });
    if let Some(perms) = cx.st.permissions {
        ui.add_space(14.0);
        card(ui, &pal, |ui| {
            ui.label(RichText::new(tr.permissions_title()).size(15.5).color(pal.text));
            ui.add_space(2.0);
            ui.label(RichText::new(tr.permissions_desc()).size(13.0).color(pal.muted));
            ui.add_space(12.0);
            let rows = [
                (tr.perm_accessibility(), perms.accessibility, Permission::Accessibility),
                (tr.perm_input_monitoring(), perms.input_monitoring, Permission::InputMonitoring),
            ];
            for (i, (name, granted, which)) in rows.into_iter().enumerate() {
                if i > 0 {
                    ui.add_space(10.0);
                    divider(ui, &pal);
                    ui.add_space(10.0);
                }
                ui.horizontal(|ui| {
                    let tone = if granted { Tone::Good } else { Tone::Bad };
                    let (dot, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                    ui.painter().circle_filled(dot.center(), 5.0, pal.tone(tone));
                    ui.label(RichText::new(name).size(15.0).color(pal.text));
                    ui.label(
                        RichText::new(if granted { tr.perm_granted() } else { tr.perm_missing() })
                            .size(13.5)
                            .color(pal.tone(tone)),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if button(ui, &pal, tr.open_settings(), Btn::Secondary).clicked() {
                            cx.send(UiCommand::OpenPermissionSettings(which));
                        }
                    });
                });
            }
        });
    }
    ui.add_space(14.0);
    card(ui, &pal, |ui| {
        action_row(ui, &pal, tr.reconfigure(), tr.reconfigure_desc(), tr.reconfigure(), Btn::Secondary, || {
            l.flow.restart();
        });
        ui.add_space(12.0);
        divider(ui, &pal);
        ui.add_space(12.0);
        action_row(ui, &pal, tr.unpair(), tr.unpair_desc(), tr.unpair(), Btn::Danger, || {
            l.confirm_unpair = true;
        });
    });
    ui.add_space(8.0);
}

/// A title with a description on the left and a button on the right.
fn action_row(
    ui: &mut Ui,
    pal: &Palette,
    title: &str,
    description: &str,
    button_text: &str,
    kind: Btn,
    on_click: impl FnOnce(),
) {
    ui.horizontal(|ui| {
        let button_w = ui
            .painter()
            .layout_no_wrap(button_text.to_owned(), FontId::proportional(15.0), pal.text)
            .size()
            .x
            + 36.0;
        let text_w = (ui.available_width() - button_w - 24.0).max(120.0);
        ui.allocate_ui_with_layout(vec2(text_w, 0.0), Layout::top_down(Align::Min), |ui| {
            ui.set_width(text_w);
            ui.spacing_mut().item_spacing.y = 3.0;
            ui.label(RichText::new(title).size(15.5).color(pal.text));
            ui.label(RichText::new(description).size(13.0).color(pal.muted));
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if button(ui, pal, button_text, kind).clicked() {
                on_click();
            }
        });
    });
}

fn unpair_dialog(ui: &mut Ui, cx: &mut Cx<'_>, l: &mut Local) {
    if !l.confirm_unpair {
        return;
    }
    let (pal, tr) = (cx.pal, cx.tr);
    let peer = cx.st.peer.as_ref().map_or("—", |p| p.name.as_str()).to_owned();
    let modal = egui::Modal::new(Id::new("unpair_dialog"))
        .frame(
            Frame::new()
                .fill(pal.card)
                .stroke(Stroke::new(1.0, pal.border_strong))
                .corner_radius(CornerRadius::same(16))
                .inner_margin(Margin::same(22)),
        )
        .show(ui.ctx(), |ui| {
            ui.set_width(380.0);
            heavy(ui, &tr.unpair_confirm(&peer), 19.0, pal.text);
            ui.add_space(8.0);
            paragraph(ui, tr.unpair_desc(), 14.5, pal.muted, Align::Min);
            ui.add_space(18.0);
            let mut confirmed = false;
            let mut cancelled = false;
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                confirmed = widgets::button_sized(ui, &pal, tr.unpair_confirm_btn(), Btn::Danger, 110.0).clicked();
                cancelled = button(ui, &pal, tr.cancel(), Btn::Secondary).clicked();
            });
            (confirmed, cancelled)
        });
    let (confirmed, cancelled) = modal.inner;
    if confirmed {
        l.confirm_unpair = false;
        cx.send(UiCommand::Unpair);
    } else if cancelled || modal.should_close() {
        l.confirm_unpair = false;
    }
}

// ---- about -------------------------------------------------------------

fn about_tab(ui: &mut Ui, cx: &mut Cx<'_>, l: &mut Local) {
    let (pal, tr) = (cx.pal, cx.tr);
    ui.add_space(6.0);
    card(ui, &pal, |ui| {
        ui.horizontal(|ui| {
            if let Some(logo) = &l.logo {
                let (rect, _) = ui.allocate_exact_size(vec2(76.0, 76.0), Sense::hover());
                ui.painter().image(
                    logo.id(),
                    rect,
                    egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                heavy(ui, "MacWin", 26.0, pal.text);
                ui.label(RichText::new(tr.version(&cx.st.app_version)).size(14.0).color(pal.muted));
                ui.label(RichText::new(tr.about_tagline()).size(14.5).color(pal.muted));
            });
        });
    });
    ui.add_space(14.0);
    card(ui, &pal, |ui| {
        ui.label(RichText::new(tr.emergency_title()).size(15.5).color(pal.text));
        ui.add_space(4.0);
        ui.label(
            RichText::new(tr.emergency_keys())
                .size(14.5)
                .monospace()
                .color(pal.tone(Tone::Wait)),
        );
    });
    ui.add_space(14.0);
    card(ui, &pal, |ui| {
        ui.label(RichText::new(tr.how_title()).size(15.5).color(pal.text));
        ui.add_space(6.0);
        for item in tr.how_items() {
            ui.horizontal_top(|ui| {
                let (dot, _) = ui.allocate_exact_size(vec2(8.0, 20.0), Sense::hover());
                ui.painter().circle_filled(dot.center(), 3.0, pal.accent);
                ui.add(egui::Label::new(RichText::new(item).size(14.5).color(pal.muted)).wrap());
            });
            ui.add_space(2.0);
        }
    });
    ui.add_space(12.0);
    let cmd = if cfg!(target_os = "macos") { "Cmd" } else { "Ctrl" };
    ui.label(RichText::new(tr.shortcuts_hint(cmd)).size(13.0).color(pal.faint));
    ui.add_space(8.0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::fixtures::desk_state;

    fn view(status: Status) -> StatusView {
        let mut st = desk_state();
        st.status = status;
        status_view(Tr(Lang::Ru), &st, None)
    }

    #[test]
    fn connected_texts_follow_who_is_driving() {
        let v = view(Status::Connected(Activity::Idle));
        assert_eq!((v.tone, v.text.as_str()), (Tone::Good, "Подключено к «Dmitriy-PC»"));
        let v = view(Status::Connected(Activity::Controlling));
        assert_eq!(v.text, "Управляете «Dmitriy-PC»");
        let v = view(Status::Connected(Activity::Controlled));
        assert_eq!(v.text, "«Dmitriy-PC» управляет этим компьютером");
    }

    #[test]
    fn waiting_states_are_amber_and_name_the_peer() {
        assert_eq!(view(Status::Listening).text, "Ожидание «Dmitriy-PC»");
        for s in [Status::Listening, Status::Connecting, Status::Retrying("x".into())] {
            assert_eq!(view(s).tone, Tone::Wait);
        }
        let v = view(Status::Retrying("timeout".into()));
        assert_eq!(v.detail.as_deref(), Some("timeout"));
    }

    #[test]
    fn paused_is_gray_and_trouble_is_red() {
        let v = view(Status::Paused);
        assert_eq!((v.tone, v.text.as_str()), (Tone::Idle, "Пауза"));
        assert_eq!(view(Status::EmergencyPaused).tone, Tone::Bad);
        let v = view(Status::Failed("порт занят".into()));
        assert_eq!((v.tone, v.detail.as_deref()), (Tone::Bad, Some("порт занят")));
    }

    #[test]
    fn the_connected_detail_names_the_crossing_edge() {
        let mut st = desk_state();
        st.status = Status::Connected(Activity::Idle);
        let v = status_view(Tr(Lang::Ru), &st, Some((Edge::Left, "MacBook")));
        assert_eq!(
            v.detail.as_deref(),
            Some("Курсор переходит через левый край «MacBook»")
        );
    }

    #[test]
    fn pause_applies_only_to_a_settled_connection() {
        assert!(pausable(&Status::Connected(Activity::Idle)));
        assert!(pausable(&Status::Paused));
        assert!(!pausable(&Status::Starting));
        assert!(!pausable(&Status::Failed("x".into())));
        assert!(paused(&Status::EmergencyPaused));
        assert!(!paused(&Status::Listening));
    }
}
