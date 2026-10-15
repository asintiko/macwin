//! The first-run wizard: role, arrangement, code, done.

use eframe::egui::{
    self, Align, CornerRadius, CursorIcon, FontId, Frame, Id, Key, Layout, Margin, Pos2, Response,
    RichText, Sense, Stroke, StrokeKind, Ui, UiBuilder, epaint::Shadow, pos2, text::LayoutJob,
    text::TextFormat, vec2,
};
use macwin_core::arrange::{Edge, Placement};

use crate::app::{Cx, Local};
use crate::code;
use crate::editor::{self, Params};
use crate::flow::Step;
use crate::glyphs;
use crate::groups;
use crate::model::{Role, Status, UiCommand, UiState};
use crate::theme::{Palette, Tone, tint};
use crate::widgets::{self, Btn, banner, button, centered_column, heavy, paragraph};

const COLUMN: f32 = 720.0;

/// The wizard page for the current step.
pub(crate) fn show(ui: &mut Ui, cx: &mut Cx<'_>, l: &mut Local) {
    let pal = cx.pal;
    let step = l.flow.step();
    egui::Panel::top("wizard_top")
        .frame(Frame::new().fill(pal.bg).inner_margin(Margin::symmetric(28, 14)))
        .show(ui, |ui| header(ui, cx, l));
    if has_footer(l, cx.st) {
        egui::Panel::bottom("wizard_bottom")
            .frame(Frame::new().fill(pal.bg).inner_margin(Margin::symmetric(28, 14)))
            .show(ui, |ui| footer(ui, cx, l));
    }
    egui::CentralPanel::default()
        .frame(Frame::new().fill(pal.bg).inner_margin(Margin::symmetric(28, 0)))
        .show(ui, |ui| match step {
            Step::Role => role_page(ui, cx, l),
            Step::Arrangement => arrangement_page(ui, cx, l),
            Step::Code if cx.st.role == Some(Role::Secondary) => show_code_page(ui, cx, l),
            Step::Code => enter_code_page(ui, cx, l),
            Step::Done => done_page(ui, cx, l),
        });
}

fn has_footer(l: &Local, st: &UiState) -> bool {
    match l.flow.step() {
        Step::Role => l.flow.rerun(),
        Step::Code => st.role.is_some(),
        _ => true,
    }
}

// ---- header ------------------------------------------------------------

fn header(ui: &mut Ui, cx: &Cx<'_>, l: &Local) {
    let (pal, tr) = (cx.pal, cx.tr);
    centered_column(ui, 900.0, |ui| {
        ui.horizontal(|ui| {
            if let Some(logo) = &l.logo {
                let (rect, _) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::hover());
                ui.painter().image(
                    logo.id(),
                    rect,
                    egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
            heavy(ui, "MacWin", 18.0, pal.text);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (index, total) = l.flow.progress(cx.st.role);
                let done = l.flow.step() == Step::Done;
                ui.spacing_mut().item_spacing.x = 5.0;
                for i in (1..=total).rev() {
                    let (rect, _) = ui.allocate_exact_size(vec2(26.0, 5.0), Sense::hover());
                    let on = done || i <= index;
                    ui.painter().rect_filled(
                        rect,
                        CornerRadius::same(3),
                        if on { pal.accent } else { pal.border_strong },
                    );
                }
                ui.add_space(8.0);
                let text = if done {
                    tr.done_title_short().to_owned()
                } else {
                    tr.step_of(index, total)
                };
                ui.label(RichText::new(text).size(13.5).color(pal.muted));
            });
        });
    });
}

// ---- footer ------------------------------------------------------------

fn footer(ui: &mut Ui, cx: &mut Cx<'_>, l: &mut Local) {
    let (pal, tr) = (cx.pal, cx.tr);
    centered_column(ui, 900.0, |ui| {
        ui.horizontal(|ui| match l.flow.step() {
            Step::Role => {
                if button(ui, &pal, tr.cancel(), Btn::Secondary).clicked() {
                    for c in l.flow.back(cx.st) {
                        cx.send(c);
                    }
                }
            }
            Step::Arrangement => {
                if button(ui, &pal, tr.back(), Btn::Secondary).clicked() {
                    for c in l.flow.back(cx.st) {
                        cx.send(c);
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::button_sized(ui, &pal, tr.next(), Btn::Primary, 120.0).clicked() {
                        let p = current_placement(cx.st, l);
                        for c in l.flow.confirm_arrangement(cx.st, p) {
                            cx.send(c);
                        }
                    }
                });
            }
            Step::Code if cx.st.role == Some(Role::Secondary) => {
                if button(ui, &pal, tr.cancel(), Btn::Secondary).clicked() {
                    for c in l.flow.cancel_pairing(cx.st) {
                        cx.send(c);
                    }
                }
            }
            Step::Code => {
                let dialing = cx.st.status == Status::PairingDial;
                if button(ui, &pal, tr.back(), Btn::Secondary).clicked() {
                    for c in l.flow.back(cx.st) {
                        cx.send(c);
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if dialing {
                        if widgets::button_sized(ui, &pal, tr.cancel(), Btn::Secondary, 140.0).clicked()
                        {
                            cx.send(UiCommand::CancelPairing);
                        }
                    } else {
                        let ready = code::is_complete(&l.code);
                        let go = ui
                            .add_enabled_ui(ready, |ui| {
                                widgets::button_sized(ui, &pal, tr.connect(), Btn::Primary, 140.0)
                            })
                            .inner;
                        if go.clicked() {
                            submit_code(cx, l);
                        }
                    }
                });
            }
            Step::Done => {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::button_sized(ui, &pal, tr.open_main(), Btn::Primary, 200.0).clicked() {
                        l.flow.finish();
                    }
                });
            }
        });
    });
}

fn submit_code(cx: &Cx<'_>, l: &Local) {
    let address = if l.address_open { l.address.as_str() } else { "" };
    if let Some(text) = code::command_text(&l.code, address) {
        cx.send(UiCommand::EnterCode(text));
    }
}

// ---- shared pieces -----------------------------------------------------

/// A title and a subtitle, centred.
fn title_block(ui: &mut Ui, pal: &Palette, title: &str, subtitle: Option<&str>) {
    ui.vertical_centered(|ui| {
        ui.spacing_mut().item_spacing.y = 10.0;
        heavy(ui, title, 28.0, pal.text);
        if let Some(s) = subtitle {
            ui.scope(|ui| {
                ui.set_max_width(580.0);
                paragraph(ui, s, 15.5, pal.muted, Align::Center);
            });
        }
    });
}

/// The arrangement shown and edited by the wizard.
fn current_placement(st: &UiState, l: &Local) -> Placement {
    l.flow
        .draft()
        .or(st.placement)
        .unwrap_or_else(|| default_placement(st))
}

/// Next to the main screen, on its right.
pub(crate) fn default_placement(st: &UiState) -> Placement {
    let display = st
        .local_screens
        .iter()
        .find(|s| s.primary)
        .or(st.local_screens.first())
        .map(|s| s.id);
    Placement {
        display,
        edge: Edge::Right,
        offset: None,
    }
}

// ---- role --------------------------------------------------------------

fn role_page(ui: &mut Ui, cx: &mut Cx<'_>, l: &mut Local) {
    let (pal, tr) = (cx.pal, cx.tr);
    centered_column(ui, COLUMN, |ui| {
        ui.add_space(30.0);
        let subtitle = tr.role_subtitle(&cx.st.device_name, cx.st.this_os);
        title_block(ui, &pal, tr.role_title(), Some(&subtitle));
        ui.add_space(28.0);
        let current = if l.flow.rerun() { cx.st.role } else { None };
        let mut chosen = None;
        ui.columns(2, |cols| {
            if role_card(&mut cols[0], &pal, Role::Primary, current == Some(Role::Primary), tr)
                .clicked()
            {
                chosen = Some(Role::Primary);
            }
            if role_card(&mut cols[1], &pal, Role::Secondary, current == Some(Role::Secondary), tr)
                .clicked()
            {
                chosen = Some(Role::Secondary);
            }
        });
        if let Some(role) = chosen {
            for c in l.flow.choose_role(cx.st, role) {
                cx.send(c);
            }
        }
    });
}

fn role_card(ui: &mut Ui, pal: &Palette, role: Role, current: bool, tr: crate::i18n::Tr) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 276.0), Sense::click());
    let hover = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.14);
    let painter = ui.painter().clone();
    let radius = CornerRadius::same(18);
    if hover > 0.0 {
        let shadow = Shadow {
            offset: [0, 8],
            blur: 24,
            spread: 0,
            color: egui::Color32::from_black_alpha((hover * if pal.dark { 110.0 } else { 36.0 }) as u8),
        };
        painter.add(shadow.as_shape(rect, radius));
    }
    painter.rect_filled(rect, radius, pal.card.lerp_to_gamma(pal.card_hover, hover));
    painter.rect_stroke(
        rect,
        radius,
        Stroke::new(1.0 + hover, pal.border_strong.lerp_to_gamma(pal.accent, hover)),
        StrokeKind::Inside,
    );
    let glyph_at = pos2(rect.center().x, rect.top() + 86.0);
    match role {
        Role::Primary => glyphs::primary(&painter, pal, glyph_at),
        Role::Secondary => glyphs::secondary(&painter, pal, glyph_at),
    }
    if current {
        let galley = painter.layout_no_wrap(
            tr.current_role().to_owned(),
            FontId::proportional(12.5),
            pal.accent,
        );
        let tag = egui::Rect::from_min_size(
            rect.right_top() + vec2(-galley.size().x - 34.0, 14.0),
            galley.size() + vec2(20.0, 8.0),
        );
        painter.rect_filled(tag, CornerRadius::same(255), pal.accent_soft());
        painter.galley(tag.center() - galley.size() / 2.0, galley, pal.accent);
    }
    let (title, desc) = match role {
        Role::Primary => (tr.primary(), tr.primary_desc()),
        Role::Secondary => (tr.secondary(), tr.secondary_desc()),
    };
    let text_rect = egui::Rect::from_min_max(
        pos2(rect.left() + 26.0, rect.top() + 156.0),
        pos2(rect.right() - 26.0, rect.bottom() - 16.0),
    );
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(text_rect)
            .layout(Layout::top_down(Align::Center)),
    );
    child.spacing_mut().item_spacing.y = 8.0;
    heavy(&mut child, title, 21.0, pal.text);
    paragraph(&mut child, desc, 14.0, pal.muted, Align::Center);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

// ---- arrangement -------------------------------------------------------

fn arrangement_page(ui: &mut Ui, cx: &mut Cx<'_>, l: &mut Local) {
    let (pal, tr) = (cx.pal, cx.tr);
    centered_column(ui, 900.0, |ui| {
        ui.add_space(20.0);
        title_block(ui, &pal, tr.arrange_title(), Some(tr.arrange_hint()));
        ui.add_space(16.0);
        let groups = groups::build(cx.st, Role::Primary, tr);
        let placement = current_placement(cx.st, l);
        let height = (ui.available_height() - 72.0).clamp(200.0, 460.0);
        let params = Params {
            groups: &groups,
            placement,
            editable: true,
            height,
        };
        if let Some(p) = l.editor.show(ui, &pal, tr, &params) {
            l.flow.set_draft(p);
        }
        ui.add_space(10.0);
        editor::legend(ui, &pal, tr, &groups);
        if let Some(text) = editor::caption(tr, &groups, &placement) {
            ui.add_space(2.0);
            ui.label(RichText::new(text).size(14.0).color(pal.text));
        }
    });
}

// ---- code: primary enters ---------------------------------------------

fn enter_code_page(ui: &mut Ui, cx: &mut Cx<'_>, l: &mut Local) {
    let (pal, tr) = (cx.pal, cx.tr);
    centered_column(ui, 640.0, |ui| {
        ui.add_space(44.0);
        title_block(ui, &pal, tr.code_title(), Some(tr.code_note()));
        ui.add_space(30.0);
        let dialing = cx.st.status == Status::PairingDial;

        let id = Id::unique("wizard_code_input");
        let mut submit = false;
        ui.vertical_centered(|ui| {
            ui.add_enabled_ui(!dialing, |ui| {
                let edit = egui::TextEdit::singleline(&mut l.code)
                    .id(id)
                    .font(FontId::monospace(40.0))
                    .align(egui::Align2::CENTER_CENTER)
                    .desired_width(300.0)
                    .margin(Margin::symmetric(14, 14))
                    .hint_text(RichText::new("000-000").color(pal.faint))
                    .frame(
                        Frame::new()
                            .fill(pal.card)
                            .stroke(Stroke::new(1.5, pal.border_strong))
                            .corner_radius(CornerRadius::same(14)),
                    );
                let resp = ui.add(edit);
                if resp.changed() {
                    let formatted = code::format_code(&l.code);
                    if formatted != l.code {
                        l.code = formatted;
                    }
                    if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), id) {
                        let end = egui::text::CCursor::new(l.code.chars().count());
                        state.cursor.set_char_range(Some(egui::text::CCursorRange::one(end)));
                        egui::TextEdit::store_state(ui.ctx(), id, state);
                    }
                }
                if resp.has_focus() {
                    ui.painter().rect_stroke(
                        resp.rect.expand(2.0),
                        CornerRadius::same(16),
                        Stroke::new(2.0, tint(pal.accent, 150)),
                        StrokeKind::Outside,
                    );
                }
                if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    submit = true;
                }
                if !dialing && !resp.has_focus() && !l.address_open && ui.ctx().memory(|m| m.focused()).is_none() {
                    resp.request_focus();
                }
            });
        });
        if submit && code::is_complete(&l.code) {
            submit_code(cx, l);
        }

        ui.add_space(18.0);
        ui.vertical_centered(|ui| {
            manual_address(ui, cx, l, dialing);
        });
        ui.add_space(16.0);
        ui.vertical_centered(|ui| status_line(ui, cx, l, dialing));
    });
}

fn manual_address(ui: &mut Ui, cx: &Cx<'_>, l: &mut Local, disabled: bool) {
    let (pal, tr) = (cx.pal, cx.tr);
    ui.add_enabled_ui(!disabled, |ui| {
        let galley = ui.painter().layout_no_wrap(
            tr.manual_address().to_owned(),
            FontId::proportional(14.5),
            pal.muted,
        );
        let size = vec2(galley.size().x + 30.0, 30.0);
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
        let color = if resp.hovered() { pal.text } else { pal.muted };
        let painter = ui.painter();
        // A chevron that turns when the section is open.
        let c = pos2(rect.left() + 9.0, rect.center().y);
        let pts = if l.address_open {
            vec![c + vec2(-4.0, -2.0), c + vec2(0.0, 2.5), c + vec2(4.0, -2.0)]
        } else {
            vec![c + vec2(-2.0, -4.0), c + vec2(2.5, 0.0), c + vec2(-2.0, 4.0)]
        };
        painter.add(egui::Shape::line(pts, Stroke::new(1.8, color)));
        painter.galley(pos2(rect.left() + 22.0, rect.center().y - galley.size().y / 2.0), galley, color);
        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            l.address_open = !l.address_open;
        }
        if l.address_open {
            ui.add_space(4.0);
            ui.add(
                egui::TextEdit::singleline(&mut l.address)
                    .hint_text(RichText::new(tr.manual_address_hint()).color(pal.faint))
                    .desired_width(360.0)
                    .margin(Margin::symmetric(12, 9))
                    .frame(
                        Frame::new()
                            .fill(pal.card)
                            .stroke(Stroke::new(1.0, pal.border_strong))
                            .corner_radius(CornerRadius::same(10)),
                    ),
            );
        }
    });
}

fn status_line(ui: &mut Ui, cx: &mut Cx<'_>, l: &mut Local, dialing: bool) {
    let (pal, tr) = (cx.pal, cx.tr);
    if dialing {
        ui.horizontal(|ui| {
            // Centre the pair by padding with half of what is left.
            let text = tr.dialing();
            let w = 22.0 + 10.0 + ui.painter().layout_no_wrap(text.to_owned(), FontId::proportional(15.0), pal.muted).size().x;
            ui.add_space(((ui.available_width() - w) / 2.0).max(0.0));
            ui.add(egui::Spinner::new().size(18.0).color(pal.accent));
            ui.label(RichText::new(text).size(15.0).color(pal.muted));
        });
    }
    if let Some(n) = cx.st.notice.clone() {
        if l.dismissed != Some(n.id) {
            ui.scope(|ui| {
                ui.set_max_width(520.0);
                if banner(ui, &pal, n.error, &n.title, &n.message, tr.dismiss()) {
                    l.dismissed = Some(n.id);
                    cx.send(UiCommand::DismissNotice(n.id));
                }
            });
        }
    }
}

// ---- code: secondary shows --------------------------------------------

fn show_code_page(ui: &mut Ui, cx: &mut Cx<'_>, l: &mut Local) {
    let (pal, tr) = (cx.pal, cx.tr);
    let _ = l;
    centered_column(ui, 640.0, |ui| {
        ui.add_space(40.0);
        title_block(ui, &pal, tr.secondary_code_title(), None);
        ui.add_space(26.0);
        let code = match &cx.st.status {
            Status::PairingWait { code } => code.clone(),
            _ => None,
        };
        let waiting = matches!(cx.st.status, Status::PairingWait { .. });
        ui.vertical_centered(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(460.0, 158.0), Sense::hover());
            let painter = ui.painter().clone();
            let radius = CornerRadius::same(22);
            painter.rect_filled(rect, radius, pal.card);
            painter.rect_stroke(rect, radius, Stroke::new(1.5, tint(pal.accent, 140)), StrokeKind::Inside);
            match &code {
                Some(c) => {
                    let mut job = LayoutJob::simple_singleline(c.clone(), FontId::monospace(68.0), pal.text);
                    if let Some(s) = job.sections.first_mut() {
                        s.format = TextFormat {
                            extra_letter_spacing: 7.0,
                            ..s.format.clone()
                        };
                    }
                    let galley = painter.layout_job(job);
                    painter.galley(rect.center() - galley.size() / 2.0, galley, pal.text);
                }
                None => {
                    let c: Pos2 = rect.center();
                    if waiting {
                        let spinner_rect = egui::Rect::from_center_size(c - vec2(0.0, 12.0), vec2(34.0, 34.0));
                        egui::Spinner::new().size(34.0).color(pal.accent).paint_at(ui, spinner_rect);
                        painter.text(
                            c + vec2(0.0, 30.0),
                            egui::Align2::CENTER_CENTER,
                            tr.generating_code(),
                            FontId::proportional(14.5),
                            pal.muted,
                        );
                    } else {
                        let r = egui::Rect::from_center_size(c, vec2(190.0, 36.0));
                        let mut child = ui.new_child(UiBuilder::new().max_rect(r));
                        if widgets::button_sized(&mut child, &pal, tr.show_code(), Btn::Primary, 190.0)
                            .clicked()
                        {
                            cx.send(UiCommand::StartPairing);
                        }
                    }
                }
            }
        });
        ui.add_space(18.0);
        ui.vertical_centered(|ui| {
            ui.scope(|ui| {
                ui.set_max_width(480.0);
                paragraph(ui, tr.code_validity(), 14.5, pal.muted, Align::Center);
            });
            ui.add_space(18.0);
            if code.is_some() {
                ui.horizontal(|ui| {
                    let text = tr.waiting_primary();
                    let w = 22.0 + 10.0 + ui.painter().layout_no_wrap(text.to_owned(), FontId::proportional(15.0), pal.muted).size().x;
                    ui.add_space(((ui.available_width() - w) / 2.0).max(0.0));
                    ui.add(egui::Spinner::new().size(18.0).color(pal.accent));
                    ui.label(RichText::new(text).size(15.0).color(pal.muted));
                });
            }
        });
        if let Some(n) = cx.st.notice.clone() {
            if l.dismissed != Some(n.id) {
                ui.add_space(14.0);
                ui.scope(|ui| {
                    ui.set_max_width(520.0);
                    if banner(ui, &pal, n.error, &n.title, &n.message, tr.dismiss()) {
                        l.dismissed = Some(n.id);
                        cx.send(UiCommand::DismissNotice(n.id));
                    }
                });
            }
        }
    });
}

// ---- done --------------------------------------------------------------

fn done_page(ui: &mut Ui, cx: &mut Cx<'_>, _l: &mut Local) {
    let (pal, tr) = (cx.pal, cx.tr);
    centered_column(ui, 640.0, |ui| {
        ui.add_space(52.0);
        ui.vertical_centered(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(120.0, 120.0), Sense::hover());
            glyphs::check(ui.painter(), rect.center(), 52.0, pal.tone(Tone::Good));
            ui.add_space(14.0);
            heavy(ui, tr.done_title(), 28.0, pal.text);
            ui.add_space(10.0);
            let peer = cx.st.peer.as_ref().map_or("", |p| p.name.as_str());
            let hint = match cx.st.role {
                Some(Role::Secondary) => tr.done_hint_secondary(peer),
                _ => {
                    let groups = groups::build(cx.st, Role::Primary, tr);
                    match cx.st.placement.or_else(|| Some(default_placement(cx.st))) {
                        Some(p) => {
                            let anchor = anchor_name(&groups, &p);
                            tr.done_hint_primary(p.edge, &anchor, peer)
                        }
                        None => String::new(),
                    }
                }
            };
            ui.scope(|ui| {
                ui.set_max_width(540.0);
                paragraph(ui, &hint, 16.0, pal.muted, Align::Center);
            });
        });
    });
}

/// What to call the primary computer's edge the cursor crosses: the
/// computer's name for a single screen, the screen's own label otherwise.
fn anchor_name(groups: &groups::Groups, p: &Placement) -> String {
    if groups.anchor.screens.len() == 1 {
        return groups.anchor.name.clone();
    }
    match macwin_core::arrange::scene(&groups.anchor.screens, &groups.moving.screens, p) {
        Some(scene) => editor::anchor_label(groups, &scene).to_owned(),
        None => groups.anchor.name.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Lang;
    use crate::model::fixtures::desk_state;

    #[test]
    fn default_placement_hangs_off_the_main_screen() {
        let st = desk_state();
        let p = default_placement(&st);
        assert_eq!((p.display, p.edge, p.offset), (Some(1), Edge::Right, None));
        let mut st = desk_state();
        st.local_screens.clear();
        assert_eq!(default_placement(&st).display, None);
    }

    #[test]
    fn the_done_hint_names_the_screen_the_cursor_crosses() {
        let st = desk_state();
        let g = groups::build(&st, Role::Primary, crate::i18n::Tr(Lang::Ru));
        let p = Placement {
            display: Some(1),
            edge: Edge::Left,
            offset: Some(30),
        };
        assert_eq!(anchor_name(&g, &p), "Встроенный дисплей");
        let mut st = desk_state();
        st.local_screens.truncate(1);
        let g = groups::build(&st, Role::Primary, crate::i18n::Tr(Lang::Ru));
        assert_eq!(anchor_name(&g, &p), "MacBook Pro");
    }
}
