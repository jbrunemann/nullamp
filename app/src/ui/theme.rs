//! The fixed Nullamp look: graphite surfaces, a phosphor LCD, one signal accent.

use egui::{Color32, CornerRadius, FontFamily, FontId, Stroke, Style, TextStyle, Visuals};

pub const BG: Color32 = Color32::from_rgb(0x12, 0x14, 0x17);
pub const PANEL: Color32 = Color32::from_rgb(0x19, 0x1C, 0x20);
pub const RAISED: Color32 = Color32::from_rgb(0x22, 0x26, 0x2B);
pub const RAISED_HOVER: Color32 = Color32::from_rgb(0x2B, 0x30, 0x37);
pub const BORDER: Color32 = Color32::from_rgb(0x2E, 0x33, 0x3A);
pub const TEXT: Color32 = Color32::from_rgb(0xD7, 0xDC, 0xE2);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x8C, 0x95, 0xA0);
pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x5C, 0x64, 0x6E);

pub const LCD_BG: Color32 = Color32::from_rgb(0x08, 0x0D, 0x0C);
pub const LCD_FG: Color32 = Color32::from_rgb(0x7C, 0xF0, 0xC8);
pub const LCD_MID: Color32 = Color32::from_rgb(0x4C, 0x9E, 0x84);
pub const LCD_OFF: Color32 = Color32::from_rgb(0x14, 0x24, 0x20);
/// The constituent track inside a mix.
pub const LCD_AMBER: Color32 = Color32::from_rgb(0xFF, 0xB5, 0x47);

/// Playback signal: the playing row, active setlist entry, play button.
pub const ACCENT: Color32 = Color32::from_rgb(0xFF, 0x8A, 0x2B);
pub const ACCENT_DIM: Color32 = Color32::from_rgb(0x6B, 0x3C, 0x16);
pub const SELECT_BG: Color32 = Color32::from_rgb(0x24, 0x34, 0x45);
pub const SELECT_STROKE: Color32 = Color32::from_rgb(0x4A, 0x7B, 0xA8);
pub const ROW_ALT: Color32 = Color32::from_rgb(0x16, 0x19, 0x1C);

pub const OK: Color32 = Color32::from_rgb(0x6C, 0xCB, 0x8C);
pub const WARN: Color32 = Color32::from_rgb(0xE5, 0xB9, 0x4C);
pub const ERR: Color32 = Color32::from_rgb(0xE5, 0x62, 0x5C);

pub const ROW_HEIGHT: f32 = 20.0;

pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

pub fn prop(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

/// Symbols the UI uses that egui's bundled fonts lack (arrows, ⌘ ⌥ ⇧, ⚠ ✓ ✎ …),
/// subset from DejaVu Sans (see assets/fonts/LICENSE-DejaVu.txt).
const SYMBOLS_FONT: &[u8] = include_bytes!("../../assets/fonts/NullampSymbols.ttf");

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert("nullamp-symbols".into(), std::sync::Arc::new(egui::FontData::from_static(SYMBOLS_FONT)));
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        fonts.families.entry(family).or_default().push("nullamp-symbols".into());
    }
    ctx.set_fonts(fonts);
}

pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);
    let mut visuals = Visuals::dark();
    visuals.panel_fill = PANEL;
    visuals.window_fill = PANEL;
    visuals.extreme_bg_color = BG;
    visuals.faint_bg_color = ROW_ALT;
    visuals.code_bg_color = LCD_BG;
    visuals.window_stroke = Stroke::new(1.0, BORDER);
    visuals.window_corner_radius = CornerRadius::same(6);
    visuals.menu_corner_radius = CornerRadius::same(4);
    visuals.selection.bg_fill = SELECT_BG;
    visuals.selection.stroke = Stroke::new(1.0, SELECT_STROKE);
    visuals.hyperlink_color = Color32::from_rgb(0x7F, 0xB8, 0xE8);
    visuals.warn_fg_color = WARN;
    visuals.error_fg_color = ERR;
    visuals.override_text_color = None;

    let w = &mut visuals.widgets;
    for (state, fill) in [
        (&mut w.noninteractive, PANEL),
        (&mut w.inactive, RAISED),
        (&mut w.hovered, RAISED_HOVER),
        (&mut w.active, Color32::from_rgb(0x33, 0x39, 0x41)),
        (&mut w.open, RAISED_HOVER),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.corner_radius = CornerRadius::same(3);
    }
    w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_DIM);
    w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    w.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    w.hovered.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    w.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x44, 0x4B, 0x55));
    w.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    w.active.bg_stroke = Stroke::new(1.0, ACCENT);

    ctx.set_visuals(visuals);
    ctx.all_styles_mut(|style: &mut Style| {
        style.spacing.item_spacing = egui::vec2(6.0, 4.0);
        style.spacing.button_padding = egui::vec2(7.0, 3.0);
        style.spacing.interact_size.y = 20.0;
        style.spacing.scroll.bar_width = 8.0;
        style.spacing.scroll.floating = true;
        style.text_styles.insert(TextStyle::Body, prop(13.0));
        style.text_styles.insert(TextStyle::Button, prop(13.0));
        style.text_styles.insert(TextStyle::Small, prop(11.0));
        style.text_styles.insert(TextStyle::Monospace, mono(12.5));
        style.text_styles.insert(TextStyle::Heading, prop(15.0));
        // Decorative animation off: state changes show immediately.
        style.animation_time = 0.06;
    });
}
