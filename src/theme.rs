//! The look of the studio: sumi ink, paper text, one vermilion accent.
//!
//! The interface stays neutral so the colours on screen are the work's.
//! Vermilion is kept for what the seed decides: the seed itself, the reroll,
//! and the draws (`~`, `choose`) in the code.

use egui::{Color32, CornerRadius, Stroke, Visuals};
use qell_core::TokenKind;

pub const INK: Color32 = Color32::from_rgb(0x17, 0x16, 0x14);
pub const PANEL: Color32 = Color32::from_rgb(0x1b, 0x1a, 0x18);
pub const BAR: Color32 = Color32::from_rgb(0x1f, 0x1e, 0x1b);
pub const WELL: Color32 = Color32::from_rgb(0x12, 0x11, 0x10);
pub const LINE: Color32 = Color32::from_rgb(0x34, 0x32, 0x2e);
pub const EDGE: Color32 = Color32::from_rgb(0x4a, 0x47, 0x41);
pub const PAPER: Color32 = Color32::from_rgb(0xe8, 0xe4, 0xda);
pub const TEXT: Color32 = Color32::from_rgb(0xcf, 0xc9, 0xbc);
pub const MUTED: Color32 = Color32::from_rgb(0xa3, 0x9d, 0x90);
pub const FAINT: Color32 = Color32::from_rgb(0x8a, 0x85, 0x7a);
pub const VERMILION: Color32 = Color32::from_rgb(0xf0, 0x69, 0x4c);
pub const BLUSH: Color32 = Color32::from_rgb(0xff, 0xb5, 0xa3);
pub const AMBER: Color32 = Color32::from_rgb(0xe0, 0xb0, 0x5a);
pub const MOSS: Color32 = Color32::from_rgb(0xb9, 0xc7, 0x9a);
pub const MIST: Color32 = Color32::from_rgb(0x8f, 0xb8, 0xc9);
pub const CLAY: Color32 = Color32::from_rgb(0xd9, 0xa9, 0x8c);

pub fn apply(ctx: &egui::Context) {
    let mut v = Visuals::dark();
    v.panel_fill = PANEL;
    v.window_fill = BAR;
    v.extreme_bg_color = PANEL;
    v.faint_bg_color = BAR;
    v.override_text_color = Some(TEXT);
    v.selection.bg_fill = Color32::from_rgb(0x5a, 0x2f, 0x25);
    v.selection.stroke = Stroke::new(1.0_f32, PAPER);
    v.hyperlink_color = BLUSH;
    v.window_stroke = Stroke::new(1.0_f32, LINE);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, LINE);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, TEXT);
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = CornerRadius::same(4);
        w.fg_stroke = Stroke::new(1.0_f32, PAPER);
    }
    v.widgets.inactive.weak_bg_fill = BAR;
    v.widgets.inactive.bg_fill = BAR;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, EDGE);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x2a, 0x29, 0x26);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, MUTED);
    v.widgets.active.weak_bg_fill = Color32::from_rgb(0x34, 0x32, 0x2e);
    ctx.set_visuals(v);

    ctx.style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 5.0);
        for (style, font) in s.text_styles.iter_mut() {
            font.size = match style {
                egui::TextStyle::Small => 11.0,
                egui::TextStyle::Heading => 16.0,
                _ => 13.0,
            };
        }
    });
}

/// The colour of a token in the code pane. `text` is the token itself: the
/// draws are keywords for the highlighter, but here they get the accent.
pub fn token(kind: TokenKind, text: &str) -> Color32 {
    use TokenKind::*;
    if matches!(text, "~" | "~~" | "choose") {
        return VERMILION;
    }
    match kind {
        Comment => FAINT,
        String => MOSS,
        Number | Constant => AMBER,
        Keyword => PAPER,
        Function => MIST,
        Type => CLAY,
        Variable => PAPER,
        Parameter | Punctuation | Operator => MUTED,
        _ => TEXT,
    }
}
