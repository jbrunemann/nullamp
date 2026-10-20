//! The fixed Nullamp look: graphite surfaces, a phosphor LCD, one signal accent.

use std::sync::atomic::{AtomicBool, Ordering};

use egui::{Color32, CornerRadius, FontFamily, FontId, Stroke, Style, TextStyle, Visuals};

/// The LCD is a device, not chrome: it stays dark in both themes.
pub const LCD_BG: Color32 = Color32::from_rgb(0x08, 0x0D, 0x0C);
pub const LCD_FG: Color32 = Color32::from_rgb(0x7C, 0xF0, 0xC8);
pub const LCD_MID: Color32 = Color32::from_rgb(0x4C, 0x9E, 0x84);
pub const LCD_OFF: Color32 = Color32::from_rgb(0x14, 0x24, 0x20);
/// The constituent track inside a mix.
pub const LCD_AMBER: Color32 = Color32::from_rgb(0xFF, 0xB5, 0x47);
pub const LCD_BORDER: Color32 = Color32::from_rgb(0x2E, 0x33, 0x3A);
pub const LCD_TEXT: Color32 = Color32::from_rgb(0xD7, 0xDC, 0xE2);
pub const LCD_TEXT_DIM: Color32 = Color32::from_rgb(0x8C, 0x95, 0xA0);
/// Playback signal on the LCD: the playhead.
pub const LCD_ACCENT: Color32 = Color32::from_rgb(0xFF, 0x8A, 0x2B);
pub const ACCENT_DIM: Color32 = Color32::from_rgb(0x6B, 0x3C, 0x16);

/// The chrome colors that differ between the dark and the light theme.
pub struct Palette {
    /// The recessed level: deck and status bar, table header, pressed buttons.
    pub bg: Color32,
    pub panel: Color32,
    pub raised: Color32,
    pub raised_hover: Color32,
    pub active: Color32,
    pub border: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub text_faint: Color32,
    pub text_strong: Color32,
    /// Playback signal: the playing row, active setlist entry, play button.
    pub accent: Color32,
    pub select_bg: Color32,
    pub select_stroke: Color32,
    pub row_alt: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub err: Color32,
    pub link: Color32,
    /// The LCD hues, as legible text on chrome surfaces.
    pub now: Color32,
    pub mix_tag: Color32,
    pub key: Color32,
}

const DARK: Palette = Palette {
    bg: Color32::from_rgb(0x12, 0x14, 0x17),
    panel: Color32::from_rgb(0x19, 0x1C, 0x20),
    raised: Color32::from_rgb(0x22, 0x26, 0x2B),
    raised_hover: Color32::from_rgb(0x2B, 0x30, 0x37),
    active: Color32::from_rgb(0x33, 0x39, 0x41),
    border: Color32::from_rgb(0x2E, 0x33, 0x3A),
    text: Color32::from_rgb(0xD7, 0xDC, 0xE2),
    text_dim: Color32::from_rgb(0x8C, 0x95, 0xA0),
    text_faint: Color32::from_rgb(0x5C, 0x64, 0x6E),
    text_strong: Color32::WHITE,
    accent: Color32::from_rgb(0xFF, 0x8A, 0x2B),
    select_bg: Color32::from_rgb(0x24, 0x34, 0x45),
    select_stroke: Color32::from_rgb(0x4A, 0x7B, 0xA8),
    row_alt: Color32::from_rgb(0x16, 0x19, 0x1C),
    ok: Color32::from_rgb(0x6C, 0xCB, 0x8C),
    warn: Color32::from_rgb(0xE5, 0xB9, 0x4C),
    err: Color32::from_rgb(0xE5, 0x62, 0x5C),
    link: Color32::from_rgb(0x7F, 0xB8, 0xE8),
    now: LCD_AMBER,
    mix_tag: LCD_MID,
    key: LCD_FG,
};

const LIGHT: Palette = Palette {
    bg: Color32::from_rgb(0xDF, 0xE3, 0xE8),
    panel: Color32::from_rgb(0xF0, 0xF2, 0xF5),
    raised: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    raised_hover: Color32::from_rgb(0xE9, 0xEC, 0xF0),
    active: Color32::from_rgb(0xD2, 0xD7, 0xDE),
    border: Color32::from_rgb(0xC0, 0xC6, 0xCE),
    text: Color32::from_rgb(0x1D, 0x22, 0x28),
    text_dim: Color32::from_rgb(0x56, 0x60, 0x70),
    text_faint: Color32::from_rgb(0x88, 0x90, 0x9B),
    text_strong: Color32::BLACK,
    accent: Color32::from_rgb(0xD9, 0x5F, 0x00),
    select_bg: Color32::from_rgb(0xCC, 0xE0, 0xF5),
    select_stroke: Color32::from_rgb(0x3F, 0x7A, 0xB5),
    row_alt: Color32::from_rgb(0xE8, 0xEB, 0xEF),
    ok: Color32::from_rgb(0x1E, 0x8E, 0x4A),
    warn: Color32::from_rgb(0xA8, 0x72, 0x00),
    err: Color32::from_rgb(0xC8, 0x37, 0x2F),
    link: Color32::from_rgb(0x0A, 0x64, 0xB4),
    now: Color32::from_rgb(0xB3, 0x5F, 0x00),
    mix_tag: Color32::from_rgb(0x2E, 0x7D, 0x69),
    key: Color32::from_rgb(0x0B, 0x7A, 0x5C),
};

/// Which palette the current frame paints with; refreshed by [`sync`].
static LIGHT_ACTIVE: AtomicBool = AtomicBool::new(false);

pub fn is_light() -> bool {
    LIGHT_ACTIVE.load(Ordering::Relaxed)
}

pub fn palette() -> &'static Palette {
    if is_light() { &LIGHT } else { &DARK }
}

/// Follow the theme egui resolved for this frame (which tracks the OS under
/// [`Appearance::System`]).
pub fn sync(ctx: &egui::Context) {
    LIGHT_ACTIVE.store(ctx.theme() == egui::Theme::Light, Ordering::Relaxed);
}

/// Run `add` with the dark palette and dark widget visuals whatever the theme is. The deck is a
/// piece of hi-fi hardware, and those were always black: it stays dark in the light theme too.
pub fn with_dark<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let was_light = LIGHT_ACTIVE.swap(false, Ordering::Relaxed);
    let outer = ui.style().clone();
    if was_light {
        ui.set_style(ui.ctx().style_of(egui::Theme::Dark));
    }
    let out = add(ui);
    ui.set_style(outer);
    LIGHT_ACTIVE.store(was_light, Ordering::Relaxed);
    out
}

/// The deck's panel fill: the dark theme's recessed level in both themes.
pub fn deck_bg() -> Color32 {
    DARK.bg
}

macro_rules! chrome_colors {
    ($($name:ident),* $(,)?) => {
        $(pub fn $name() -> Color32 { palette().$name })*
    };
}
chrome_colors!(
    bg, panel, raised, raised_hover, border, text, text_dim, text_faint, text_strong, accent, select_bg, select_stroke, row_alt, ok, warn, err,
    now, mix_tag, key,
);

/// The colour of a key on the Camelot wheel: twelve hues in even steps, so neighbours on the
/// wheel (the keys that mix well) look alike. The letter does not change it.
pub fn camelot_color(number: u8) -> Color32 {
    let hue = (170.0 - f32::from(number.clamp(1, 12) - 1) * 30.0).rem_euclid(360.0) / 360.0;
    let (saturation, value) = if is_light() { (0.80, 0.62) } else { (0.55, 0.95) };
    egui::ecolor::Hsva::new(hue, saturation, value, 1.0).into()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Appearance {
    System,
    #[default]
    Dark,
    Light,
}

impl Appearance {
    pub fn from_setting(s: &str) -> Self {
        match s {
            "system" => Appearance::System,
            "light" => Appearance::Light,
            _ => Appearance::Dark,
        }
    }

    pub fn setting(self) -> &'static str {
        match self {
            Appearance::System => "system",
            Appearance::Dark => "dark",
            Appearance::Light => "light",
        }
    }

    fn preference(self) -> egui::ThemePreference {
        match self {
            Appearance::System => egui::ThemePreference::System,
            Appearance::Dark => egui::ThemePreference::Dark,
            Appearance::Light => egui::ThemePreference::Light,
        }
    }
}

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

fn visuals(base: Visuals, p: &Palette) -> Visuals {
    let dark = base.dark_mode;
    let mut visuals = base;
    visuals.panel_fill = p.panel;
    visuals.window_fill = p.panel;
    visuals.extreme_bg_color = if dark { p.bg } else { p.raised };
    visuals.faint_bg_color = p.row_alt;
    visuals.code_bg_color = LCD_BG;
    visuals.window_stroke = Stroke::new(1.0, p.border);
    visuals.window_corner_radius = CornerRadius::same(6);
    visuals.menu_corner_radius = CornerRadius::same(4);
    visuals.selection.bg_fill = p.select_bg;
    visuals.selection.stroke = Stroke::new(1.0, p.select_stroke);
    visuals.hyperlink_color = p.link;
    visuals.warn_fg_color = p.warn;
    visuals.error_fg_color = p.err;
    visuals.override_text_color = None;

    let w = &mut visuals.widgets;
    for (state, fill) in [
        (&mut w.noninteractive, p.panel),
        (&mut w.inactive, p.raised),
        (&mut w.hovered, p.raised_hover),
        (&mut w.active, p.active),
        (&mut w.open, p.raised_hover),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.corner_radius = CornerRadius::same(3);
    }
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text_dim);
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.bg_stroke = Stroke::new(1.0, p.border);
    w.hovered.fg_stroke = Stroke::new(1.0, p.text_strong);
    w.hovered.bg_stroke = Stroke::new(1.0, if dark { Color32::from_rgb(0x44, 0x4B, 0x55) } else { Color32::from_rgb(0xA4, 0xAC, 0xB7) });
    w.active.fg_stroke = Stroke::new(1.0, p.text_strong);
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    visuals
}

/// Width reserved beside content for a floating scroll bar, where the bar must not cover buttons or fields.
pub const SCROLL_GUTTER: f32 = 10.0;

/// Install fonts, both themes' visuals and the chosen appearance. Both themes get
/// full visuals so that following the OS never falls back to egui's stock look.
pub fn apply(ctx: &egui::Context, appearance: Appearance) {
    install_fonts(ctx);
    ctx.set_visuals_of(egui::Theme::Dark, visuals(Visuals::dark(), &DARK));
    ctx.set_visuals_of(egui::Theme::Light, visuals(Visuals::light(), &LIGHT));
    set_appearance(ctx, appearance);
    ctx.all_styles_mut(|style: &mut Style| {
        style.spacing.item_spacing = egui::vec2(6.0, 4.0);
        style.spacing.button_padding = egui::vec2(7.0, 3.0);
        style.spacing.interact_size.y = 20.0;
        style.spacing.scroll.bar_width = 8.0;
        style.spacing.scroll.floating = true;
        // The handle is drawn in the text colour, which glares unless it stays translucent.
        style.spacing.scroll.dormant_handle_opacity = 0.15;
        style.spacing.scroll.active_handle_opacity = 0.25;
        style.spacing.scroll.interact_handle_opacity = 0.45;
        style.text_styles.insert(TextStyle::Body, prop(13.0));
        style.text_styles.insert(TextStyle::Button, prop(13.0));
        style.text_styles.insert(TextStyle::Small, prop(11.0));
        style.text_styles.insert(TextStyle::Monospace, mono(12.5));
        style.text_styles.insert(TextStyle::Heading, prop(15.0));
        // Decorative animation off: state changes show immediately.
        style.animation_time = 0.06;
    });
}

pub fn set_appearance(ctx: &egui::Context, appearance: Appearance) {
    ctx.set_theme(appearance.preference());
    sync(ctx);
}
