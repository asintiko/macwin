//! Little pictures drawn with the painter: laptops, a cursor, a check mark.

use eframe::egui::{
    Color32, CornerRadius, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, pos2, vec2,
};

use crate::theme::{Palette, tint};

/// A laptop of about 120 x 86 points centred on `c`; returns its display.
fn laptop(painter: &Painter, c: Pos2, line: Color32, fill: Color32) -> Rect {
    let screen = Rect::from_center_size(c + vec2(0.0, -7.0), vec2(92.0, 58.0));
    painter.rect_filled(screen, CornerRadius::same(8), fill);
    painter.rect_stroke(
        screen,
        CornerRadius::same(8),
        Stroke::new(3.0, line),
        StrokeKind::Inside,
    );
    let base = Rect::from_center_size(c + vec2(0.0, 31.0), vec2(118.0, 7.0));
    let lip = CornerRadius {
        nw: 1,
        ne: 1,
        sw: 4,
        se: 4,
    };
    painter.rect_filled(base, lip, line);
    let notch = Rect::from_center_size(c + vec2(0.0, 28.5), vec2(26.0, 3.0));
    painter.rect_filled(notch, CornerRadius::same(1), tint(Color32::BLACK, 70));
    screen
}

/// A mouse cursor arrow whose tip is at `tip`.
pub fn cursor(painter: &Painter, tip: Pos2, scale: f32, fill: Color32, outline: Color32) {
    let pts: Vec<Pos2> = [
        (0.0, 0.0),
        (0.0, 17.0),
        (4.2, 13.2),
        (7.2, 20.0),
        (10.4, 18.6),
        (7.4, 12.0),
        (12.8, 12.0),
    ]
    .iter()
    .map(|&(x, y)| tip + vec2(x, y) * scale)
    .collect();
    painter.add(Shape::convex_polygon(pts.clone(), fill, Stroke::NONE));
    painter.add(Shape::closed_line(
        pts,
        Stroke::new(1.4 * scale.max(1.0), outline),
    ));
}

/// The "primary" computer: a laptop with the cursor on its screen.
pub fn primary(painter: &Painter, pal: &Palette, c: Pos2) {
    let screen = laptop(painter, c, pal.accent, tint(pal.accent, 38));
    cursor(
        painter,
        screen.center() + vec2(6.0, -9.0),
        1.5,
        Color32::WHITE,
        Color32::from_rgb(0x0b, 0x10, 0x20),
    );
}

/// The "secondary" computer: a laptop with a code on its screen.
pub fn secondary(painter: &Painter, pal: &Palette, c: Pos2) {
    let screen = laptop(painter, c, pal.muted, tint(pal.muted, 26));
    for side in [-1.0_f32, 1.0] {
        for k in -1..=1 {
            let x = screen.center().x + side * 24.0 + k as f32 * 9.0;
            painter.circle_filled(pos2(x, screen.center().y), 3.4, pal.accent);
        }
    }
}

/// A check mark in a circle of radius `r`.
pub fn check(painter: &Painter, c: Pos2, r: f32, color: Color32) {
    painter.circle_filled(c, r, tint(color, 40));
    painter.circle_filled(c, r * 0.78, color);
    let k = r / 40.0;
    let pts = vec![
        c + vec2(-14.0, 1.0) * k,
        c + vec2(-4.0, 11.0) * k,
        c + vec2(15.0, -10.0) * k,
    ];
    painter.add(Shape::line(
        pts,
        Stroke::new(5.0 * k.max(0.5), Color32::WHITE),
    ));
}
