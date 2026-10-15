//! Colours, text sizes and the egui style.

use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Theme, Vec2, Visuals,
};
use macwin_core::types::Os;

/// How a status reads: the colour family it is drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Everything works.
    Good,
    /// Waiting for something.
    Wait,
    /// Switched off on purpose.
    Idle,
    /// Something is wrong.
    Bad,
}

/// All colours of the window for one theme.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    /// Dark theme.
    pub dark: bool,
    /// Window background.
    pub bg: Color32,
    /// Raised surfaces.
    pub card: Color32,
    /// A card under the pointer.
    pub card_hover: Color32,
    /// Hairlines around cards.
    pub border: Color32,
    /// Hairlines around controls.
    pub border_strong: Color32,
    /// Main text.
    pub text: Color32,
    /// Secondary text.
    pub muted: Color32,
    /// Hints and disabled text.
    pub faint: Color32,
    /// Controls and selection.
    pub accent: Color32,
    /// Accent under the pointer.
    pub accent_hover: Color32,
    /// Text on an accent fill.
    pub on_accent: Color32,
    /// Arrangement canvas background.
    pub canvas: Color32,
    /// Dots of the canvas grid.
    pub canvas_dot: Color32,
    /// Fill of the primary computer's screens.
    pub screen_fill: Color32,
    /// Outline of those screens.
    pub screen_stroke: Color32,
    /// The menu bar / task bar strip of a main display.
    pub screen_bar: Color32,
    /// Label on a screen.
    pub screen_text: Color32,
    /// Secondary label on a screen.
    pub screen_text_dim: Color32,
    /// The lines where the cursor crosses.
    pub cross: Color32,
    /// A macOS computer.
    pub mac: Color32,
    /// A Windows computer.
    pub windows: Color32,
    good: Color32,
    wait: Color32,
    idle: Color32,
    bad: Color32,
}

fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

impl Palette {
    /// The palette for the dark or the light theme.
    pub fn new(dark: bool) -> Palette {
        if dark {
            Palette {
                dark,
                bg: rgb(0x0e1420),
                card: rgb(0x161e2d),
                card_hover: rgb(0x1b2538),
                border: rgb(0x263247),
                border_strong: rgb(0x34435f),
                text: rgb(0xe7ecf6),
                muted: rgb(0x93a0b8),
                faint: rgb(0x64728a),
                accent: rgb(0x3b82f6),
                accent_hover: rgb(0x5b9aff),
                on_accent: Color32::WHITE,
                canvas: rgb(0x0a0f19),
                canvas_dot: rgb(0x1b2536),
                screen_fill: rgb(0x1f2a3f),
                screen_stroke: rgb(0x3d4e6e),
                screen_bar: rgb(0x2b3953),
                screen_text: rgb(0xe3e9f5),
                screen_text_dim: rgb(0x8d9ab3),
                cross: rgb(0xffc531),
                mac: rgb(0x2f6df0),
                windows: rgb(0x0b9f73),
                good: rgb(0x34d27b),
                wait: rgb(0xf5a524),
                idle: rgb(0x93a0b8),
                bad: rgb(0xff6b61),
            }
        } else {
            Palette {
                dark,
                bg: rgb(0xf2f5fa),
                card: rgb(0xffffff),
                card_hover: rgb(0xf6f8fd),
                border: rgb(0xdde4f0),
                border_strong: rgb(0xc5cfe2),
                text: rgb(0x101828),
                muted: rgb(0x566178),
                faint: rgb(0x8a94a8),
                accent: rgb(0x2563eb),
                accent_hover: rgb(0x1d4fd8),
                on_accent: Color32::WHITE,
                canvas: rgb(0xe4eaf5),
                canvas_dot: rgb(0xccd5e6),
                screen_fill: rgb(0xffffff),
                screen_stroke: rgb(0xaab8d2),
                screen_bar: rgb(0xe3e9f4),
                screen_text: rgb(0x1b2536),
                screen_text_dim: rgb(0x66738c),
                cross: rgb(0xf5a000),
                mac: rgb(0x2563eb),
                windows: rgb(0x0a8f67),
                good: rgb(0x15803d),
                wait: rgb(0xb45309),
                idle: rgb(0x4b5563),
                bad: rgb(0xc62828),
            }
        }
    }

    /// The palette of the theme `ui` is drawn in.
    pub fn of(ui: &egui::Ui) -> Palette {
        Palette::new(ui.visuals().dark_mode)
    }

    /// The text and dot colour of a status tone.
    pub fn tone(&self, tone: Tone) -> Color32 {
        match tone {
            Tone::Good => self.good,
            Tone::Wait => self.wait,
            Tone::Idle => self.idle,
            Tone::Bad => self.bad,
        }
    }

    /// The tinted background of a status pill.
    pub fn tone_bg(&self, tone: Tone) -> Color32 {
        tint(self.tone(tone), if self.dark { 34 } else { 28 })
    }

    /// The colour a computer of `os` is drawn in.
    pub fn os_tint(&self, os: Os) -> Color32 {
        match os {
            Os::MacOs => self.mac,
            Os::Windows => self.windows,
        }
    }

    /// A pale accent fill, e.g. behind a selected card.
    pub fn accent_soft(&self) -> Color32 {
        tint(self.accent, if self.dark { 30 } else { 22 })
    }
}

/// `c` with the given alpha (0..=255), unmultiplied.
pub fn tint(c: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha)
}

fn visuals(p: &Palette) -> Visuals {
    let mut v = if p.dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    v.panel_fill = p.bg;
    v.window_fill = p.card;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(14);
    v.extreme_bg_color = p.canvas;
    v.faint_bg_color = p.card_hover;
    v.hyperlink_color = p.accent;
    v.selection.bg_fill = tint(p.accent, 90);
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.text_cursor.stroke = Stroke::new(2.0, p.accent);

    let radius = CornerRadius::same(8);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.card;
    w.noninteractive.weak_bg_fill = p.card;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = radius;
    w.inactive.bg_fill = p.card_hover;
    w.inactive.weak_bg_fill = p.card_hover;
    w.inactive.bg_stroke = Stroke::new(1.0, p.border_strong);
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.corner_radius = radius;
    w.hovered.bg_fill = p.card_hover;
    w.hovered.weak_bg_fill = p.card_hover;
    w.hovered.bg_stroke = Stroke::new(1.0, p.accent);
    w.hovered.fg_stroke = Stroke::new(1.0, p.text);
    w.hovered.corner_radius = radius;
    w.hovered.expansion = 0.0;
    w.active.bg_fill = p.card_hover;
    w.active.weak_bg_fill = p.card_hover;
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    w.active.fg_stroke = Stroke::new(1.0, p.text);
    w.active.corner_radius = radius;
    w.active.expansion = 0.0;
    w.open.corner_radius = radius;
    v
}

/// Installs the fonts sizes, spacing and colours for both themes.
pub fn install(ctx: &egui::Context) {
    ctx.set_visuals_of(Theme::Dark, visuals(&Palette::new(true)));
    ctx.set_visuals_of(Theme::Light, visuals(&Palette::new(false)));
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Heading, FontId::new(26.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(15.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(15.0, FontFamily::Proportional)),
            (TextStyle::Small, FontId::new(12.5, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(15.0, FontFamily::Monospace)),
        ]
        .into();
        style.spacing.item_spacing = Vec2::new(10.0, 8.0);
        style.spacing.button_padding = Vec2::new(14.0, 7.0);
        style.spacing.interact_size = Vec2::new(40.0, 32.0);
        style.interaction.selectable_labels = false;
        style.animation_time = 0.12;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tone_has_its_own_colour_in_both_themes() {
        for dark in [true, false] {
            let p = Palette::new(dark);
            let tones = [Tone::Good, Tone::Wait, Tone::Idle, Tone::Bad].map(|t| p.tone(t));
            for (i, a) in tones.iter().enumerate() {
                for b in &tones[i + 1..] {
                    assert_ne!(a, b);
                }
            }
        }
    }

    #[test]
    fn tint_keeps_the_colour_and_sets_the_alpha() {
        let c = tint(Color32::from_rgb(10, 20, 30), 77);
        assert_eq!((c.r(), c.g(), c.b(), c.a()), (10, 20, 30, 77));
    }
}
