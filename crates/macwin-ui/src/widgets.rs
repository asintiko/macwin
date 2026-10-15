//! Small painted widgets shared by the wizard and the main window.

use std::time::Duration;

use eframe::egui::{
    self, Align, Color32, CornerRadius, CursorIcon, FontId, InnerResponse, Layout, Margin, Pos2,
    Rect, Response, RichText, Sense, Stroke, StrokeKind, Ui, UiBuilder, pos2, text::LayoutJob,
    vec2,
};

use crate::theme::{Palette, Tone, tint};

/// What a button means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Btn {
    /// The main action of a screen.
    Primary,
    /// Any other action.
    Secondary,
    /// A destructive action.
    Danger,
}

/// `c` with its channels scaled by `k` (opaque colours only).
pub fn darken(c: Color32, k: f32) -> Color32 {
    let s = |v: u8| (f32::from(v) * k).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(s(c.r()), s(c.g()), s(c.b()))
}

fn focus_ring(ui: &Ui, pal: &Palette, rect: Rect, radius: u8) {
    ui.painter().rect_stroke(
        rect.expand(2.0),
        CornerRadius::same(radius + 2),
        Stroke::new(2.0, tint(pal.accent, 140)),
        StrokeKind::Outside,
    );
}

/// A button of height 36 and at least `min_width` pixels.
pub fn button_sized(ui: &mut Ui, pal: &Palette, text: &str, kind: Btn, min_width: f32) -> Response {
    let enabled = ui.is_enabled();
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        FontId::proportional(15.0),
        Color32::PLACEHOLDER,
    );
    let size = vec2((galley.size().x + 36.0).max(min_width), 36.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let hover = ui
            .ctx()
            .animate_bool_with_time(resp.id, resp.hovered() && enabled, 0.12);
        let pressed = resp.is_pointer_button_down_on() && enabled;
        let (fill, stroke, fg) = match kind {
            Btn::Primary => {
                let base = pal.accent;
                let mut f = base.lerp_to_gamma(pal.accent_hover, hover);
                if pressed {
                    f = darken(base, 0.88);
                }
                (f, Stroke::NONE, pal.on_accent)
            }
            Btn::Secondary => (
                pal.card_hover.lerp_to_gamma(pal.border, hover * 0.6),
                Stroke::new(1.0, pal.border_strong.lerp_to_gamma(pal.accent, hover)),
                pal.text,
            ),
            Btn::Danger => (
                tint(pal.tone(Tone::Bad), (hover * 34.0) as u8),
                Stroke::new(1.0, pal.tone(Tone::Bad)),
                pal.tone(Tone::Bad),
            ),
        };
        let radius = CornerRadius::same(9);
        let painter = ui.painter();
        painter.rect_filled(rect, radius, fill);
        if stroke.width > 0.0 {
            painter.rect_stroke(rect, radius, stroke, StrokeKind::Inside);
        }
        let pos = rect.center() - galley.size() / 2.0;
        painter.galley(pos, galley, fg);
        if resp.has_focus() {
            focus_ring(ui, pal, rect, 9);
        }
    }
    if enabled {
        resp.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        resp
    }
}

/// A button sized to its text.
pub fn button(ui: &mut Ui, pal: &Palette, text: &str, kind: Btn) -> Response {
    button_sized(ui, pal, text, kind, 0.0)
}

/// An on/off switch; flips `on` when clicked.
pub fn toggle(ui: &mut Ui, pal: &Palette, on: &mut bool) -> Response {
    let enabled = ui.is_enabled();
    let (rect, mut resp) = ui.allocate_exact_size(vec2(44.0, 26.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_with_time(resp.id, *on, 0.14);
        let bg = pal.border_strong.lerp_to_gamma(pal.accent, t);
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(13), bg);
        let r = if resp.is_pointer_button_down_on() {
            11.0
        } else {
            10.0
        };
        let x = egui::lerp((rect.left() + 13.0)..=(rect.right() - 13.0), t);
        let c = pos2(x, rect.center().y);
        painter.circle_filled(c + vec2(0.0, 1.0), r, Color32::from_black_alpha(50));
        painter.circle_filled(c, r, Color32::WHITE);
        if resp.has_focus() {
            focus_ring(ui, pal, rect, 13);
        }
    }
    if enabled {
        resp.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        resp
    }
}

/// A card: a raised, bordered surface as wide as the available space.
pub fn card<R>(
    ui: &mut Ui,
    pal: &Palette,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> InnerResponse<R> {
    egui::Frame::new()
        .fill(pal.card)
        .stroke(Stroke::new(1.0, pal.border))
        .corner_radius(CornerRadius::same(14))
        .inner_margin(Margin::same(18))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add_contents(ui)
        })
}

/// A hairline across the available width.
pub fn divider(ui: &mut Ui, pal: &Palette) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, pal.border);
}

/// Text with a stronger weight than the single bundled font has: drawn twice
/// with a sub-pixel offset.
pub fn heavy(ui: &mut Ui, text: &str, size: f32, color: Color32) -> Response {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), FontId::proportional(size), color);
    let extra = (size / 40.0).clamp(0.3, 0.7);
    let (rect, resp) = ui.allocate_exact_size(galley.size() + vec2(extra, 0.0), Sense::hover());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.galley(rect.min, galley.clone(), color);
        painter.galley(rect.min + vec2(extra, 0.0), galley, color);
    }
    resp
}

/// Wrapped text.
pub fn paragraph(ui: &mut Ui, text: &str, size: f32, color: Color32, align: Align) -> Response {
    ui.add(egui::Label::new(RichText::new(text).size(size).color(color)).halign(align))
}

/// A status pill: a dot and a text on a tinted background, at most
/// `max_width` wide (a longer text is cut with an ellipsis).
pub fn pill(ui: &mut Ui, pal: &Palette, tone: Tone, text: &str, max_width: f32) -> Response {
    let fg = pal.tone(tone);
    let mut job = LayoutJob::simple_singleline(text.to_owned(), FontId::proportional(15.0), fg);
    job.wrap.max_width = (max_width - 40.0).max(40.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let galley = ui.painter().layout_job(job);
    let size = vec2(galley.size().x + 22.0 + 18.0, 32.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(16), pal.tone_bg(tone));
        let dot = pos2(rect.left() + 16.0, rect.center().y);
        painter.circle_filled(dot, 8.0, tint(fg, 50));
        painter.circle_filled(dot, 4.5, fg);
        painter.galley(
            pos2(rect.left() + 30.0, rect.center().y - galley.size().y / 2.0),
            galley,
            fg,
        );
    }
    resp
}

/// A small badge with an outline.
pub fn badge(ui: &mut Ui, pal: &Palette, text: &str) -> Response {
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_owned(), FontId::proportional(13.0), pal.muted);
    let size = galley.size() + vec2(20.0, 10.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.rect_stroke(
            rect,
            CornerRadius::same(255),
            Stroke::new(1.0, pal.border_strong),
            StrokeKind::Inside,
        );
        painter.galley(rect.center() - galley.size() / 2.0, galley, pal.muted);
    }
    resp
}

/// A row of tabs with an underline; returns true when `current` changed.
pub fn tab_bar<T: Copy + PartialEq>(
    ui: &mut Ui,
    pal: &Palette,
    items: &[(T, &str)],
    current: &mut T,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (id, label) in items {
            let selected = *id == *current;
            let galley = ui.painter().layout_no_wrap(
                (*label).to_owned(),
                FontId::proportional(15.5),
                Color32::PLACEHOLDER,
            );
            let size = vec2(galley.size().x + 28.0, 42.0);
            let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
            let hover = ui
                .ctx()
                .animate_bool_with_time(resp.id, resp.hovered(), 0.12);
            let color = if selected {
                pal.text
            } else {
                pal.muted.lerp_to_gamma(pal.text, hover)
            };
            if ui.is_rect_visible(rect) {
                let painter = ui.painter();
                if hover > 0.0 && !selected {
                    painter.rect_filled(
                        rect.shrink2(vec2(2.0, 5.0)),
                        CornerRadius::same(8),
                        tint(pal.muted, (hover * 22.0) as u8),
                    );
                }
                painter.galley(
                    rect.center() - galley.size() / 2.0 - vec2(0.0, 1.0),
                    galley,
                    color,
                );
                if selected {
                    let bar = Rect::from_min_max(
                        pos2(rect.left() + 10.0, rect.bottom() - 3.0),
                        pos2(rect.right() - 10.0, rect.bottom()),
                    );
                    painter.rect_filled(bar, CornerRadius::same(2), pal.accent);
                }
                if resp.has_focus() {
                    focus_ring(ui, pal, rect.shrink2(vec2(2.0, 4.0)), 8);
                }
            }
            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() && !selected {
                *current = *id;
                changed = true;
            }
        }
    });
    changed
}

/// A segmented control; returns the index clicked, if any.
pub fn segmented(ui: &mut Ui, pal: &Palette, labels: &[&str], selected: usize) -> Option<usize> {
    let mut clicked = None;
    let height = 34.0;
    let widths: Vec<f32> = labels
        .iter()
        .map(|l| {
            ui.painter()
                .layout_no_wrap(
                    (*l).to_owned(),
                    FontId::proportional(15.0),
                    Color32::PLACEHOLDER,
                )
                .size()
                .x
                + 32.0
        })
        .collect();
    let total: f32 = widths.iter().sum::<f32>() + 8.0;
    let (rect, _) = ui.allocate_exact_size(vec2(total, height + 8.0), Sense::hover());
    let painter = ui.painter().clone();
    painter.rect_filled(rect, CornerRadius::same(11), pal.canvas);
    painter.rect_stroke(
        rect,
        CornerRadius::same(11),
        Stroke::new(1.0, pal.border),
        StrokeKind::Inside,
    );
    let mut x = rect.left() + 4.0;
    for (i, label) in labels.iter().enumerate() {
        let r = Rect::from_min_size(pos2(x, rect.top() + 4.0), vec2(widths[i], height));
        x += widths[i];
        let resp = ui.interact(r, resp_id(ui, i), Sense::click());
        let on = i == selected;
        if on {
            painter.rect_filled(r, CornerRadius::same(8), pal.accent_soft());
            painter.rect_stroke(
                r,
                CornerRadius::same(8),
                Stroke::new(1.0, tint(pal.accent, 150)),
                StrokeKind::Inside,
            );
        }
        let color = if on || resp.hovered() {
            pal.text
        } else {
            pal.muted
        };
        let galley = painter.layout_no_wrap((*label).to_owned(), FontId::proportional(15.0), color);
        painter.galley(r.center() - galley.size() / 2.0, galley, color);
        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() && !on {
            clicked = Some(i);
        }
    }
    clicked
}

fn resp_id(ui: &Ui, i: usize) -> egui::Id {
    ui.scope_id().with(("segmented", i))
}

/// A row with a title, an optional description and a switch on the right.
/// Returns true when the switch was flipped.
pub fn toggle_row(
    ui: &mut Ui,
    pal: &Palette,
    title: &str,
    description: Option<&str>,
    value: &mut bool,
    enabled: bool,
) -> bool {
    let mut changed = false;
    ui.add_enabled_ui(enabled, |ui| {
        let total = ui.available_width();
        let text_w = (total - 44.0 - 24.0).max(120.0);
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(vec2(text_w, 0.0), Layout::top_down(Align::Min), |ui| {
                ui.set_width(text_w);
                ui.spacing_mut().item_spacing.y = 3.0;
                ui.label(RichText::new(title).size(15.5).color(pal.text));
                if let Some(d) = description {
                    ui.label(RichText::new(d).size(13.0).color(pal.muted));
                }
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if toggle(ui, pal, value).changed() {
                    changed = true;
                }
            });
        });
    });
    changed
}

/// Paints a spinning arc in `rect` and schedules the next frame (about 30 a
/// second, which is plenty for a spinner and spares the CPU).
pub fn paint_spinner(ui: &Ui, pal: &Palette, rect: Rect) {
    let c = rect.center();
    let width = (rect.width() / 9.0).clamp(2.0, 3.5);
    let r = rect.width().min(rect.height()) / 2.0 - width;
    let painter = ui.painter();
    painter.circle_stroke(c, r, Stroke::new(width, tint(pal.accent, 45)));
    let start = (ui.input(|i| i.time) * 5.0) as f32;
    let sweep = std::f32::consts::PI * 1.3;
    let steps = 24;
    let points: Vec<Pos2> = (0..=steps)
        .map(|i| {
            let a = start + sweep * i as f32 / steps as f32;
            c + vec2(a.cos(), a.sin()) * r
        })
        .collect();
    painter.add(egui::Shape::line(points, Stroke::new(width, pal.accent)));
    ui.ctx().request_repaint_after(Duration::from_millis(33));
}

/// A spinner of `size` points.
pub fn spinner(ui: &mut Ui, pal: &Palette, size: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    if ui.is_rect_visible(rect) {
        paint_spinner(ui, pal, rect);
    }
    resp
}

/// A round "×" button; `hint` is its tooltip.
pub fn close_button(ui: &mut Ui, pal: &Palette, hint: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let hover = ui
            .ctx()
            .animate_bool_with_time(resp.id, resp.hovered(), 0.12);
        let painter = ui.painter();
        painter.circle_filled(rect.center(), 14.0, tint(pal.muted, (hover * 50.0) as u8));
        let color = pal.muted.lerp_to_gamma(pal.text, hover);
        let d = 4.5;
        let c = rect.center();
        for (a, b) in [(vec2(-d, -d), vec2(d, d)), (vec2(-d, d), vec2(d, -d))] {
            painter.line_segment([c + a, c + b], Stroke::new(1.8, color));
        }
        if resp.has_focus() {
            focus_ring(ui, pal, rect, 14);
        }
    }
    resp.on_hover_text(hint)
        .on_hover_cursor(CursorIcon::PointingHand)
}

/// A dismissible banner; returns true when its close button was clicked.
pub fn banner(
    ui: &mut Ui,
    pal: &Palette,
    error: bool,
    title: &str,
    message: &str,
    close: &str,
) -> bool {
    let tone = if error { Tone::Bad } else { Tone::Wait };
    let color = pal.tone(tone);
    let mut dismissed = false;
    egui::Frame::new()
        .fill(pal.tone_bg(tone))
        .stroke(Stroke::new(1.0, tint(color, 90)))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::symmetric(16, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let text_w = (ui.available_width() - 44.0).max(100.0);
                ui.allocate_ui_with_layout(vec2(text_w, 0.0), Layout::top_down(Align::Min), |ui| {
                    ui.set_width(text_w);
                    ui.spacing_mut().item_spacing.y = 2.0;
                    if !title.is_empty() {
                        heavy(ui, title, 15.0, color);
                    }
                    if !message.is_empty() {
                        ui.label(RichText::new(message).size(14.0).color(pal.text));
                    }
                });
                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                    if close_button(ui, pal, close).clicked() {
                        dismissed = true;
                    }
                });
            });
        });
    dismissed
}

/// Adds space above content of `content_height` so it sits a little above the
/// middle of the available height (never less than 20 points).
pub fn space_to_centre(ui: &mut Ui, content_height: f32) {
    ui.add_space(((ui.available_height() - content_height) * 0.4).max(20.0));
}

/// Runs `add_contents` in a column of at most `max_width`, centred in the
/// available space.
pub fn centered_column<R>(
    ui: &mut Ui,
    max_width: f32,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> R {
    let avail = ui.available_rect_before_wrap();
    let w = avail.width().min(max_width);
    let rect = Rect::from_min_size(
        pos2(avail.center().x - w / 2.0, avail.top()),
        vec2(w, avail.height()),
    );
    ui.scope_builder(
        UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::top_down(Align::Min)),
        add_contents,
    )
    .inner
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn darken_scales_channels() {
        let c = darken(Color32::from_rgb(100, 200, 50), 0.5);
        assert_eq!((c.r(), c.g(), c.b()), (50, 100, 25));
        assert_eq!(darken(Color32::WHITE, 2.0), Color32::WHITE);
    }
}
