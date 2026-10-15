//! The arrangement editor: the primary's screens stand still, the
//! secondary's screens are a group that is dragged next to them, and the lines
//! where the cursor crosses are highlighted.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, CursorIcon, FontId, Key, Painter, Pos2, Rect, Sense,
    Shape, Stroke, StrokeKind, Ui, epaint::Shadow, pos2, vec2,
};
use macwin_core::arrange::{self, LogicalScreen, Placement, RectF, Scene, bounds};
use macwin_core::types::PointF;

use crate::canvas::{self, Drag, Transform};
use crate::groups::{Group, Groups};
use crate::i18n::Tr;
use crate::theme::{Palette, tint};

/// Spacing of the canvas grid dots, in pixels.
const GRID: f32 = 22.0;
/// How long a dropped group takes to glide into place.
const SETTLE_SECS: f64 = 0.18;

/// What the editor needs for one frame.
pub struct Params<'a> {
    /// The two computers' screens and labels.
    pub groups: &'a Groups,
    /// Where the secondary sits next to the primary.
    pub placement: Placement,
    /// The user may drag the group.
    pub editable: bool,
    /// The canvas height in points.
    pub height: f32,
}

/// A drag in progress.
#[derive(Debug, Clone, Copy)]
struct Active {
    drag: Drag,
    /// The group's top-left under the pointer.
    want: PointF,
    /// Where dropping now would put it.
    target: Option<(Placement, PointF)>,
}

/// A dropped group gliding into its snapped place.
#[derive(Debug, Clone, Copy)]
struct Settle {
    from: PointF,
    to: PointF,
    placement: Placement,
    t0: f64,
}

/// Interaction state that lives between frames.
#[derive(Debug, Default)]
pub struct EditorState {
    active: Option<Active>,
    settle: Option<Settle>,
}

fn corner(r: Rect) -> CornerRadius {
    CornerRadius::same((r.width().min(r.height()) * 0.045).clamp(3.0, 8.0).round() as u8)
}

fn rects(screens: &[LogicalScreen]) -> Vec<RectF> {
    screens.iter().map(|s| s.rect).collect()
}

fn top_left(screens: &[LogicalScreen]) -> PointF {
    let b = bounds(rects(screens));
    PointF::new(b.x, b.y)
}

fn size_text(r: RectF) -> String {
    format!("{} × {}", r.w.round() as i64, r.h.round() as i64)
}

/// Draws the two lines of a label centred in `rect`, as large as fits.
fn label(painter: &Painter, rect: Rect, name: &str, details: &[String], fg: Color32, dim: Color32) {
    let (pad_x, pad_y) = (8.0, 6.0);
    let fits = |w: f32, h: f32| w <= rect.width() - 2.0 * pad_x && h <= rect.height() - 2.0 * pad_y;
    for scale in [1.0_f32, 0.88, 0.76] {
        let name_g = painter.layout_no_wrap(name.to_owned(), FontId::proportional(13.5 * scale), fg);
        let mut options: Vec<&str> = details.iter().map(String::as_str).collect();
        options.push("");
        for detail in options {
            let detail_g = (!detail.is_empty()).then(|| {
                painter.layout_no_wrap(detail.to_owned(), FontId::proportional(11.5 * scale), dim)
            });
            let w = detail_g
                .as_ref()
                .map_or(name_g.size().x, |g| g.size().x.max(name_g.size().x));
            let gap = 3.0;
            let h = name_g.size().y + detail_g.as_ref().map_or(0.0, |g| g.size().y + gap);
            if !fits(w, h) {
                continue;
            }
            let mut y = rect.center().y - h / 2.0;
            painter.galley(pos2(rect.center().x - name_g.size().x / 2.0, y), name_g.clone(), fg);
            y += name_g.size().y + gap;
            if let Some(g) = detail_g {
                painter.galley(pos2(rect.center().x - g.size().x / 2.0, y), g, dim);
            }
            return;
        }
    }
}

fn paint_background(painter: &Painter, rect: Rect, pal: &Palette) {
    let radius = CornerRadius::same(14);
    painter.rect_filled(rect, radius, pal.canvas);
    let clipped = painter.with_clip_rect(rect.shrink(1.0));
    let mut y = rect.top() + GRID * 0.5;
    while y < rect.bottom() {
        let mut x = rect.left() + GRID * 0.5;
        while x < rect.right() {
            clipped.circle_filled(pos2(x, y), 1.0, pal.canvas_dot);
            x += GRID;
        }
        y += GRID;
    }
    painter.rect_stroke(rect, radius, Stroke::new(1.0, pal.border), StrokeKind::Inside);
}

fn paint_anchor(painter: &Painter, pal: &Palette, tr: Tr, group: &Group, tf: Transform) {
    for (s, name) in group.screens.iter().zip(&group.labels) {
        let r = tf.rect_to_canvas(s.rect);
        let radius = corner(r);
        if pal.dark {
            let shadow = Shadow {
                offset: [0, 3],
                blur: 10,
                spread: 0,
                color: Color32::from_black_alpha(70),
            };
            painter.add(shadow.as_shape(r, radius));
        }
        painter.rect_filled(r, radius, pal.screen_fill);
        let mut top = r.top();
        if s.primary {
            // The menu bar (macOS) or the task bar (Windows) of the main display.
            let bar_h = (r.height() * 0.045).clamp(3.0, 7.0);
            let bar = match group.os {
                macwin_core::types::Os::MacOs => {
                    top += bar_h;
                    Rect::from_min_size(r.min, vec2(r.width(), bar_h))
                }
                macwin_core::types::Os::Windows => {
                    Rect::from_min_size(pos2(r.left(), r.bottom() - bar_h), vec2(r.width(), bar_h))
                }
            };
            let r2 = radius.nw;
            let bar_radius = match group.os {
                macwin_core::types::Os::MacOs => CornerRadius {
                    nw: r2,
                    ne: r2,
                    sw: 0,
                    se: 0,
                },
                macwin_core::types::Os::Windows => CornerRadius {
                    nw: 0,
                    ne: 0,
                    sw: r2,
                    se: r2,
                },
            };
            painter.rect_filled(bar, bar_radius, pal.screen_bar);
        }
        painter.rect_stroke(r, radius, Stroke::new(1.0, pal.screen_stroke), StrokeKind::Inside);
        let mut details = Vec::new();
        if s.primary {
            details.push(format!("{} · {}", size_text(s.rect), tr.screen_main()));
        }
        details.push(size_text(s.rect));
        let area = Rect::from_min_max(pos2(r.left(), top), r.max);
        label(painter, area, name, &details, pal.screen_text, pal.screen_text_dim);
    }
}

/// The screens of the moving group at `origin`, drawn in the group's colour.
/// `alpha` fades a ghost; `lift` adds a bigger shadow.
fn paint_moving(
    painter: &Painter,
    pal: &Palette,
    group: &Group,
    tf: Transform,
    origin: PointF,
    alpha: u8,
    lift: bool,
    grip: bool,
) {
    let placed = canvas::group_at(&group.screens, origin);
    let base = pal.os_tint(group.os);
    let main = placed.iter().position(|s| s.primary).unwrap_or(0);
    for (i, (s, name)) in placed.iter().zip(&group.labels).enumerate() {
        let r = tf.rect_to_canvas(s.rect);
        let radius = corner(r);
        let shadow = Shadow {
            offset: [0, if lift { 8 } else { 3 }],
            blur: if lift { 22 } else { 10 },
            spread: 0,
            color: Color32::from_black_alpha(if lift { 110 } else { 70 }),
        };
        painter.add(shadow.as_shape(r, radius));
        painter.rect_filled(r, radius, tint(base, alpha));
        let edge = Color32::WHITE.gamma_multiply(if pal.dark { 0.38 } else { 0.55 });
        painter.rect_stroke(r, radius, Stroke::new(1.5, edge.gamma_multiply(f32::from(alpha) / 255.0)), StrokeKind::Inside);
        let details = vec![size_text(s.rect)];
        let white = Color32::WHITE.gamma_multiply(f32::from(alpha) / 255.0);
        let dim = Color32::from_white_alpha((f32::from(alpha) * 0.72) as u8);
        label(painter, r, name, &details, white, dim);
        if grip && i == main && r.width() > 90.0 {
            let c = pos2(r.center().x, r.top() + 9.0);
            for dx in [-3.0_f32, 3.0] {
                for dy in [-3.0_f32, 0.0, 3.0] {
                    painter.circle_filled(c + vec2(dx, dy - 1.0), 1.1, Color32::from_white_alpha(120));
                }
            }
        }
    }
}

fn paint_target(painter: &Painter, pal: &Palette, group: &Group, tf: Transform, origin: PointF) {
    let placed = canvas::group_at(&group.screens, origin);
    let base = pal.os_tint(group.os);
    let line = Stroke::new(1.8, tint(base.lerp_to_gamma(Color32::WHITE, 0.45), 230));
    for s in &placed {
        let r = tf.rect_to_canvas(s.rect);
        painter.rect_filled(r, corner(r), tint(base, 46));
        let path = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
        painter.extend(Shape::dashed_line(&path, line, 7.0, 5.0));
    }
}

fn paint_contacts(painter: &Painter, pal: &Palette, contacts: &[arrange::Contact], tf: Transform) {
    for c in contacts {
        let (a, b) = (tf.to_canvas(c.from), tf.to_canvas(c.to));
        painter.line_segment([a, b], Stroke::new(11.0, tint(pal.cross, 48)));
        painter.line_segment([a, b], Stroke::new(4.5, pal.cross));
        painter.circle_filled(a, 2.25, pal.cross);
        painter.circle_filled(b, 2.25, pal.cross);
    }
}

fn paint_empty(painter: &Painter, rect: Rect, pal: &Palette, text: &str) {
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        FontId::proportional(15.0),
        pal.muted,
    );
}

/// The label of the screen the placement is attached to.
pub fn anchor_label<'g>(groups: &'g Groups, scene: &Scene) -> &'g str {
    groups
        .anchor
        .labels
        .get(scene.anchor)
        .map_or("", String::as_str)
}

impl EditorState {
    /// True while the group is being dragged.
    pub fn dragging(&self) -> bool {
        self.active.is_some()
    }

    /// Draws the editor and handles dragging. Returns the placement the user
    /// just dropped the group on, if it differs from `p.placement`.
    pub fn show(&mut self, ui: &mut Ui, pal: &Palette, tr: Tr, p: &Params<'_>) -> Option<Placement> {
        let sense = if p.editable { Sense::drag() } else { Sense::hover() };
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), p.height), sense);
        let painter = ui.painter_at(rect);
        paint_background(&painter, rect, pal);

        let anchor = &p.groups.anchor;
        let moving = &p.groups.moving;
        let Some(scene) = arrange::scene(&anchor.screens, &moving.screens, &p.placement) else {
            paint_empty(&painter, rect, pal, tr.no_screens());
            self.active = None;
            return None;
        };
        let peer_size = canvas::group_size(&moving.screens);
        let view = canvas::reachable_bounds(&rects(&anchor.screens), peer_size);
        let tf = Transform::fit(view, rect, 10.0);
        let committed = top_left(&scene.peer);
        let group_rects: Vec<Rect> = scene.peer.iter().map(|s| tf.rect_to_canvas(s.rect)).collect();
        let ctx = ui.ctx().clone();

        let mut dropped = None;
        if p.editable {
            let pointer = resp.interact_pointer_pos().or_else(|| ctx.input(|i| i.pointer.latest_pos()));
            if resp.drag_started() {
                let press = ctx.input(|i| i.pointer.press_origin());
                if let (Some(press), Some(pointer)) = (press, pointer) {
                    if canvas::hit(&group_rects, press, 4.0) {
                        self.settle = None;
                        self.active = Some(Active {
                            drag: Drag {
                                origin: committed,
                                grab: tf.to_logical(pointer),
                            },
                            want: committed,
                            target: None,
                        });
                    }
                }
            }
            if let Some(active) = self.active.as_mut() {
                if ctx.input(|i| i.key_pressed(Key::Escape)) {
                    self.active = None;
                } else {
                    if let Some(pointer) = pointer {
                        active.want = active.drag.want(tf.to_logical(pointer));
                        active.target = canvas::drop_target(&anchor.screens, peer_size, active.want);
                    }
                    if resp.drag_stopped() {
                        let done = *active;
                        self.active = None;
                        if let Some((placement, at)) = done.target {
                            self.settle = Some(Settle {
                                from: done.want,
                                to: at,
                                placement,
                                t0: ctx.input(|i| i.time),
                            });
                            if placement != p.placement {
                                dropped = Some(placement);
                            }
                        }
                    }
                }
            }
            if self.active.is_some() {
                ctx.set_cursor_icon(CursorIcon::Grabbing);
            } else if resp.hover_pos().is_some_and(|h| canvas::hit(&group_rects, h, 4.0)) {
                ctx.set_cursor_icon(CursorIcon::Grab);
            }
        }

        paint_anchor(&painter, pal, tr, anchor, tf);

        if let Some(active) = self.active {
            if let Some((_, at)) = active.target {
                paint_target(&painter, pal, moving, tf, at);
                let preview = canvas::group_at(&moving.screens, at);
                paint_contacts(&painter, pal, &canvas::crossings(&anchor.screens, &preview), tf);
            }
            paint_moving(&painter, pal, moving, tf, active.want, 215, true, false);
        } else {
            let now = ctx.input(|i| i.time);
            let mut origin = committed;
            if let Some(s) = self.settle {
                let t = ((now - s.t0) / SETTLE_SECS) as f32;
                if t >= 1.0 || s.placement != p.placement {
                    self.settle = None;
                } else {
                    origin = canvas::lerp_point(s.from, s.to, f64::from(canvas::ease_out(t)));
                    ctx.request_repaint();
                }
            }
            paint_contacts(&painter, pal, &scene.contacts, tf);
            paint_moving(&painter, pal, moving, tf, origin, 255, false, p.editable);
        }
        dropped
    }
}

/// The line under the canvas: what the colours mean.
pub fn legend(ui: &mut Ui, pal: &Palette, tr: Tr, groups: &Groups) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(18.0, 6.0);
        let item = |ui: &mut Ui, swatch: Box<dyn Fn(&Painter, Rect)>, size: egui::Vec2, text: &str| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 7.0;
                let (r, _) = ui.allocate_exact_size(size, Sense::hover());
                swatch(ui.painter(), r);
                ui.label(egui::RichText::new(text).size(13.0).color(pal.muted));
            });
        };
        let cross = pal.cross;
        item(
            ui,
            Box::new(move |p, r| {
                p.rect_filled(r, CornerRadius::same(2), cross);
            }),
            vec2(20.0, 5.0),
            tr.legend_cross(),
        );
        let moving_tint = pal.os_tint(groups.moving.os);
        let (moving_text, anchor_text) = if groups.moving_is_this {
            (tr.this_computer(), tr.legend_primary())
        } else {
            (tr.legend_second(), tr.this_computer())
        };
        item(
            ui,
            Box::new(move |p, r| {
                p.rect_filled(r, CornerRadius::same(3), moving_tint);
            }),
            vec2(16.0, 11.0),
            moving_text,
        );
        let (fill, stroke) = (pal.screen_fill, pal.screen_stroke);
        item(
            ui,
            Box::new(move |p, r| {
                p.rect_filled(r, CornerRadius::same(3), fill);
                p.rect_stroke(r, CornerRadius::same(3), Stroke::new(1.0, stroke), StrokeKind::Inside);
            }),
            vec2(16.0, 11.0),
            anchor_text,
        );
    });
}

/// "«PC» — to the left of «MacBook»" for the current placement.
pub fn caption(tr: Tr, groups: &Groups, placement: &Placement) -> Option<String> {
    let scene = arrange::scene(&groups.anchor.screens, &groups.moving.screens, placement)?;
    Some(tr.placement_caption(
        &groups.moving.name,
        placement.edge,
        anchor_label(groups, &scene),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::fixtures::desk_state;
    use crate::model::{Lang, Role};
    use macwin_core::arrange::Edge;

    #[test]
    fn caption_names_the_edge_and_the_anchor_screen() {
        let st = desk_state();
        let tr = Tr(Lang::Ru);
        let groups = crate::groups::build(&st, Role::Primary, tr);
        let p = Placement {
            display: Some(1),
            edge: Edge::Left,
            offset: Some(30),
        };
        assert_eq!(
            caption(tr, &groups, &p).unwrap(),
            "«Dmitriy-PC» — слева от «Встроенный дисплей»"
        );
        let p = Placement {
            display: Some(3),
            edge: Edge::Top,
            offset: None,
        };
        assert_eq!(
            caption(tr, &groups, &p).unwrap(),
            "«Dmitriy-PC» — над «Dell U2720Q»"
        );
    }

    #[test]
    fn size_text_rounds() {
        assert_eq!(size_text(RectF::new(0.0, 0.0, 1512.0, 981.6)), "1512 × 982");
    }
}
