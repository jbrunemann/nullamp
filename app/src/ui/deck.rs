//! The deck: LCD display, artwork, mix timeline and transport controls.

use egui::emath::GuiRounding;
use egui::{Align2, Color32, Rect, RichText, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};
use nullamp_audio::OutputDevice;
use nullamp_core::command::{Command, CommandSource};
use nullamp_core::setlist::Timing;
use nullamp_core::time::{format_clock, format_precise};
use nullamp_core::RepeatMode;

use super::widgets::{self, Icon};
use super::{App, SideTab, theme};
use crate::player::Status;

const LCD_HEIGHT: f32 = 106.0;

pub fn show(app: &mut App, ui: &mut Ui) {
    let full = ui.available_width();
    ui.horizontal(|ui| {
        let art_size = LCD_HEIGHT;
        let lcd_w = (full - art_size - 8.0).max(200.0);
        let (lcd, _) = ui.allocate_exact_size(vec2(lcd_w, LCD_HEIGHT), Sense::hover());
        lcd_display(app, ui, lcd);
        let (art_rect, art_resp) = ui.allocate_exact_size(vec2(art_size, art_size), Sense::click());
        artwork(app, ui, art_rect);
        let retro = app.settings.retro_artwork && app.now.retro_art.is_some();
        let hint = match (retro, app.now.show_original_art) {
            (true, false) => "Retro artwork · click to see the original",
            (true, true) => "Original artwork · click to go back to retro",
            (false, _) => "Artwork · click to open the inspector",
        };
        if art_resp.on_hover_text(hint).clicked() {
            if retro {
                app.now.show_original_art = !app.now.show_original_art;
            } else {
                app.toggle_side(SideTab::Inspector);
            }
        }
    });
    ui.add_space(6.0);
    timeline(app, ui);
    ui.add_space(6.0);
    transport(app, ui);
}

fn lcd_display(app: &mut App, ui: &mut Ui, rect: Rect) {
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 5.0, theme::LCD_BG);
    p.rect_stroke(rect, 5.0, Stroke::new(1.0, theme::LCD_BORDER), StrokeKind::Inside);
    // Faint scanlines for the display texture.
    let mut y = rect.top() + 2.0;
    while y < rect.bottom() {
        p.line_segment([pos2(rect.left() + 1.0, y), pos2(rect.right() - 1.0, y)], Stroke::new(1.0, Color32::from_black_alpha(40)));
        y += 3.0;
    }

    let status = app.player.status;
    let pos = app.player.position();
    let dur = app.player.duration();
    let inner = rect.shrink2(vec2(12.0, 9.0));

    // Status glyph.
    let glyph_rect = Rect::from_min_size(inner.left_top(), vec2(12.0, 12.0));
    match status {
        Status::Playing => widgets::paint_icon(&p, glyph_rect, Icon::Play, theme::LCD_FG),
        Status::Paused => widgets::paint_icon(&p, glyph_rect, Icon::Pause, if blink(ui) { theme::LCD_FG } else { theme::LCD_OFF }),
        Status::Loading => widgets::paint_icon(&p, glyph_rect, Icon::Play, theme::LCD_MID),
        Status::Stopped => widgets::paint_icon(&p, glyph_rect, Icon::Stop, theme::LCD_MID),
    }

    // Big time readout (click toggles elapsed / remaining).
    let digits_h = 34.0;
    let (time_text, shown_secs) = match (app.show_remaining, dur) {
        (true, Some(d)) => (format!("-{}", lcd_clock(d - pos)), d - pos),
        _ => (format!(" {}", lcd_clock(pos)), pos),
    };
    let time_origin = pos2(inner.left(), inner.top() + 2.0);
    let w = widgets::seven_segment_width("-188:88", digits_h);
    // Unlit "8"s behind the digits for the LCD look.
    // The first minute cell is a "1"-only digit, so its ghost shows just those two segments.
    let ghost = format!("{}188:88", &time_text[..1]);
    widgets::seven_segment(&p, time_origin, digits_h, &ghost, theme::LCD_OFF, Color32::TRANSPARENT);
    let lit = if status == Status::Stopped { theme::LCD_MID } else { theme::LCD_FG };
    // Past the display's range the digits blink slowly, like an old stereo's error state.
    if shown_secs < LCD_MAX_SECS as f64 || slow_blink(ui) {
        widgets::seven_segment(&p, time_origin, digits_h, &time_text, lit, Color32::TRANSPARENT);
    }
    let time_rect = Rect::from_min_size(time_origin, vec2(w, digits_h));
    let resp = ui.interact(time_rect, egui::Id::unique("lcd-time"), Sense::click());
    if resp.on_hover_text("Click: elapsed / remaining").clicked() {
        app.show_remaining = !app.show_remaining;
    }

    // Stream format line under the time.
    let fmt = app.player.current.as_ref().and_then(|c| c.info.as_ref()).map(|i| {
        let ch = match i.channels {
            1 => "MONO".to_owned(),
            2 => "STEREO".to_owned(),
            n => format!("{n} CH"),
        };
        let kbps = i.bitrate.map(|b| format!("{b} KBPS · ")).unwrap_or_default();
        format!("{} · {kbps}{:.1} KHZ · {ch}", i.codec, i.sample_rate as f64 / 1000.0)
    });
    let fmt_rect = p.text(
        pos2(inner.left(), inner.bottom()),
        Align2::LEFT_BOTTOM,
        fmt.unwrap_or_else(|| "NO SIGNAL".into()),
        theme::mono(10.0),
        theme::LCD_MID,
    );

    // Visualiser between the time and the format line.
    let viz = Rect::from_min_max(pos2(inner.left(), time_origin.y + digits_h + 7.0), pos2(time_origin.x + w, inner.bottom() - 17.0));
    super::visualizer::show(app, ui, viz);

    // Text area to the right of the time.
    let text_left = time_origin.x + w + 18.0;
    let text_rect = Rect::from_min_max(pos2(text_left, inner.top()), inner.right_bottom());
    if text_rect.width() < 60.0 {
        return;
    }
    let time = ui.input(|i| i.time);
    let (line1, line1_color, line2, line3) = lcd_lines(app);
    let l1 = Rect::from_min_size(text_rect.left_top(), vec2(text_rect.width(), 22.0));
    let paused = app.settings.marquee_paused;
    let mut toggle = widgets::marquee(ui, l1, &line1, theme::mono(16.0), line1_color, time, paused);
    let row2 = Rect::from_min_size(pos2(text_rect.left(), l1.bottom() + 5.0), vec2(text_rect.width(), 16.0));
    let bpm_width = bpm_display(app, ui, &p, row2);
    let l2 = Rect::from_min_size(row2.left_top(), vec2(row2.width() - bpm_width, row2.height()));
    toggle |= widgets::marquee(ui, l2, &line2, theme::mono(12.0), theme::LCD_MID, time, paused);
    if bpm_width > 0.0 {
        // The text runs under the tempo box: fade it out instead of cutting it off.
        fade_out_right(&p, rect, Rect::from_min_max(pos2(l2.right() - TEXT_FADE, l2.top()), l2.right_bottom()));
    }
    if toggle {
        app.settings.marquee_paused = !paused;
        app.mark_settings_dirty();
    }
    let l3 = p.text(pos2(text_rect.left(), l2.bottom() + 8.0), Align2::LEFT_TOP, line3, theme::mono(10.0), theme::LCD_MID);
    output_label(app, ui, &p, pos2(text_rect.right(), l3.top()), (text_rect.right() - l3.right().max(text_rect.left()) - 14.0).max(0.0));

    // Mode indicators, right-aligned on the bottom line.
    let mut x = text_rect.right();
    let indicators = [
        ("MUTE", app.player.muted),
        (match app.player.nav.repeat { RepeatMode::One => "RPT 1", _ => "RPT" }, app.player.nav.repeat != RepeatMode::Off),
        ("SHUF", app.player.nav.shuffle()),
    ];
    for (label, on) in indicators {
        let g = p.layout_no_wrap(label.to_owned(), theme::mono(10.0), if on { theme::LCD_FG } else { theme::LCD_OFF });
        x -= g.size().x;
        p.galley(pos2(x, inner.bottom() - g.size().y), g, theme::LCD_FG);
        x -= 10.0;
    }

    // File name, quietly, in the space left of the indicators.
    if let Some(path) = app.player.current_entry().map(|e| e.path.clone()) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        // The format line can run under the text column; start after it.
        let left = text_rect.left().max(fmt_rect.right() + 14.0);
        let width = x - 6.0 - left;
        if width > 30.0 {
            let mut job = egui::text::LayoutJob::simple_singleline(name, theme::mono(10.0), theme::LCD_MID.gamma_multiply(0.75));
            job.wrap = egui::text::TextWrapping::truncate_at_width(width);
            let g = ui.fonts_mut(|f| f.layout_job(job));
            let r = Rect::from_min_size(pos2(left, inner.bottom() - g.size().y), g.size());
            p.galley(r.min, g, theme::LCD_MID);
            ui.interact(r, egui::Id::unique("lcd-file"), Sense::hover()).on_hover_text(path.display().to_string());
        }
    }
}

/// Width over which text running under the tempo box fades out.
const TEXT_FADE: f32 = 26.0;

/// Cover `edge` (inside the display `lcd`) with a gradient from clear on its left to the display's
/// background on its right, keeping the scanlines the background has.
fn fade_out_right(p: &egui::Painter, lcd: Rect, edge: Rect) {
    let clear = Color32::TRANSPARENT;
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(edge.left_top(), clear);
    mesh.colored_vertex(edge.right_top(), theme::LCD_BG);
    mesh.colored_vertex(edge.right_bottom(), theme::LCD_BG);
    mesh.colored_vertex(edge.left_bottom(), clear);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    p.add(mesh);
    let mut y = lcd.top() + 2.0;
    while y < edge.top() {
        y += 3.0;
    }
    while y < edge.bottom() {
        p.line_segment([pos2(edge.left(), y), pos2(edge.right(), y)], Stroke::new(1.0, Color32::from_black_alpha(40)));
        y += 3.0;
    }
}

/// The tempo in a green frame at the right end of line 2, above the audio output, like the BPM counter
/// of a nineties stereo. Unlit digits show while a track is loaded and no tempo is known
/// (yet). Returns the width taken from line 1, 0 when nothing is drawn.
fn bpm_display(app: &App, ui: &Ui, p: &egui::Painter, row: Rect) -> f32 {
    const PAD: f32 = 4.0;
    // Space between the digits, paid for out of the padding so the frame keeps its width.
    const DIGIT_GAP: f32 = 1.2;
    // The frame is as tall as the row it sits in.
    let digits = row.height() - 6.0;
    if app.player.current_entry().is_none() {
        return 0.0;
    }
    let off = app.current_bpm_off();
    let bpm = app.current_bpm();
    let text = if off { "---".to_owned() } else { bpm.map(|(bpm, _)| super::format_bpm(bpm)).unwrap_or_default() };
    // Room for three digits, and a decimal when there is one.
    let ghost: String = if text.contains('.') { "888.8".into() } else { "888".into() };
    // Whole pixel sizes of at least 9: smaller or fractional text is smudged on 1x displays.
    let label = p.layout_no_wrap("BPM".into(), theme::mono(9.0), theme::LCD_MID);
    let tilde = p.layout_no_wrap("~".into(), theme::mono(10.0), theme::LCD_MID);
    let digits_w = widgets::seven_segment_width_spaced(&ghost, digits, DIGIT_GAP);
    let width = PAD + label.size().x + 4.6 + tilde.size().x + digits_w + PAD;
    if row.width() - width < 120.0 {
        return 0.0;
    }
    let frame = Rect::from_min_size(pos2(row.right() - width, row.top()), vec2(width, row.height()));
    p.rect_stroke(frame, 3.0, Stroke::new(1.0, theme::LCD_FG.gamma_multiply(0.55)), StrokeKind::Inside);
    p.galley(pos2(frame.left() + PAD, frame.center().y - label.size().y / 2.0), label.clone(), theme::LCD_MID);
    let digits_x = frame.right() - PAD - digits_w;
    let origin = pos2(digits_x, frame.top() + 3.0);
    let ghost_cells: String = ghost.chars().map(|c| if c == '.' { '.' } else { '8' }).collect();
    widgets::seven_segment_spaced(p, origin, digits, &ghost_cells, theme::LCD_OFF, Color32::TRANSPARENT, DIGIT_GAP);
    if off || bpm.is_some() {
        let detected = bpm.is_some_and(|(_, detected)| detected);
        // Right-aligned in the cells, so a two-digit tempo leaves its first cell unlit.
        let used = widgets::seven_segment_width_spaced(&text, digits, DIGIT_GAP);
        widgets::seven_segment_spaced(p, pos2(digits_x + digits_w - used, origin.y), digits, &text, theme::LCD_FG, Color32::TRANSPARENT, DIGIT_GAP);
        if detected {
            p.galley(pos2(digits_x - tilde.size().x, frame.center().y - tilde.size().y / 2.0), tilde, theme::LCD_MID);
        }
    }
    let tip = match bpm {
        _ if off => "Tempo switched off in the file's tags (BPM -1)",
        Some((_, true)) => "Tempo found by listening to the audio",
        Some((_, false)) => "Tempo from the file's tags",
        None => "Tempo: none found yet",
    };
    ui.interact(frame, ui.scope_id().with("lcd-bpm"), Sense::hover()).on_hover_text(tip);
    width + 8.0
}

/// The audio output in small print at the right end of line 3; click to choose another.
fn output_label(app: &mut App, ui: &mut Ui, p: &egui::Painter, top_right: egui::Pos2, max_width: f32) {
    const ICON: f32 = 11.0;
    if max_width < 40.0 {
        return;
    }
    let text = match &app.output {
        Some(o) => format!("OUT {}", abbreviate_device(&o.name)),
        None => "OUT NONE".into(),
    };
    let mut job = egui::text::LayoutJob::simple_singleline(text, theme::mono(10.0), theme::LCD_MID);
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_width - ICON - 4.0);
    let g = ui.fonts_mut(|f| f.layout_job(job));
    let r = Rect::from_min_size(pos2(top_right.x - g.size().x, top_right.y), g.size());
    let icon = Rect::from_center_size(pos2(r.left() - 4.0 - ICON / 2.0, r.center().y), vec2(ICON, ICON));
    // A stable id, so the popup's open state survives from frame to frame.
    let resp = ui.interact(r.union(icon).expand2(vec2(2.0, 3.0)), ui.scope_id().with("lcd-output"), Sense::click());
    let color = if app.output.is_none() { theme::LCD_OFF } else if resp.hovered() { theme::LCD_MID } else { theme::LCD_MID.gamma_multiply(0.75) };
    p.galley(r.min, g, color);
    widgets::paint_output_icon(p, icon, app.output.as_ref().map(|o| o.kind).unwrap_or_default(), color);
    let tip = match &app.output {
        Some(o) => format!(
            "Audio output: {} · {:.1} kHz · {} ch{}\nClick to choose another",
            o.name,
            o.sample_rate as f64 / 1000.0,
            o.channels,
            if o.fell_back { "\nThe chosen device is not available; playing on the system default" } else { "" }
        ),
        None => "No audio output device\nClick to choose one".into(),
    };
    if resp.clicked() {
        app.player.engine.request_output_devices();
    }
    resp.clone().on_hover_text(tip);
    egui::Popup::menu(&resp).show(|ui| output_menu(app, ui));
}

/// The device picker: one row per output, real hardware first and in full colour,
/// virtual devices dimmed under a divider.
fn output_menu(app: &mut App, ui: &mut Ui) {
    let chosen = app.settings.output_device.clone();
    let devices = app.output_devices.clone();
    let default = devices.iter().find(|d| d.is_default);
    let default_label = default.map(|d| format!("System default ({})", d.name)).unwrap_or_else(|| "System default".into());
    let rows: Vec<&OutputDevice> = devices.iter().filter(|d| d.kind.is_hardware()).chain(devices.iter().filter(|d| !d.kind.is_hardware())).collect();

    let font = theme::prop(13.0);
    let widest = std::iter::once(&default_label).chain(rows.iter().map(|d| &d.name)).map(|t| ui.painter().layout_no_wrap(t.clone(), font.clone(), Color32::WHITE).size().x).fold(0.0, f32::max);
    // Rows fill the width they are given, so fix it: the menu would otherwise stretch across the window.
    ui.set_width((widest + ROW_PAD * 2.0 + ROW_ICON + 14.0).clamp(240.0, 440.0));

    ui.label(RichText::new("Audio output").small().weak());
    if device_row(ui, &default_label, default.map(|d| d.kind).unwrap_or_default(), true, chosen.is_none()) {
        app.choose_output(None);
    }
    ui.separator();
    let mut divided = false;
    for d in rows {
        if !d.kind.is_hardware() && !divided {
            divided = true;
            ui.separator();
            ui.label(RichText::new("Virtual").small().weak());
        }
        if device_row(ui, &d.name, d.kind, d.kind.is_hardware(), chosen.as_deref() == Some(d.id.as_str())) {
            app.choose_output(Some(d.id.clone()));
        }
    }
}

const ROW_PAD: f32 = 6.0;
const ROW_ICON: f32 = 15.0;

/// A full-width menu row with an icon. Returns whether it was clicked.
fn device_row(ui: &mut Ui, label: &str, kind: nullamp_audio::OutputKind, bright: bool, selected: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::click());
    let visuals = ui.visuals();
    let fill = if selected {
        visuals.selection.bg_fill
    } else if resp.hovered() {
        visuals.widgets.hovered.bg_fill
    } else {
        Color32::TRANSPARENT
    };
    let text_color = if selected { visuals.selection.stroke.color } else if bright { visuals.text_color() } else { visuals.weak_text_color() };
    let p = ui.painter();
    p.rect_filled(rect, 3.0, fill);
    let icon = Rect::from_center_size(pos2(rect.left() + ROW_PAD + ROW_ICON / 2.0, rect.center().y), Vec2::splat(ROW_ICON));
    widgets::paint_output_icon(p, icon, kind, text_color);
    // The menu width is capped, so long names (Linux reports things like "Ellesmere HDMI Audio
    // [Radeon RX 470/480 / 570/580/590] Digital Stereo (HDMI 3)") end in an ellipsis instead of
    // running past the border; the full name is in the tooltip.
    let text_x = icon.right() + 8.0;
    let mut job = egui::text::LayoutJob::simple_singleline(label.to_owned(), theme::prop(13.0), text_color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(rect.right() - ROW_PAD - text_x);
    let g = ui.fonts_mut(|f| f.layout_job(job));
    let truncated = g.elided;
    p.galley(pos2(text_x, rect.center().y - g.size().y / 2.0), g, text_color);
    let resp = if truncated { resp.on_hover_text(label) } else { resp };
    resp.clicked()
}

/// Upper case with plain quotes, like the rest of the display's small print.
fn abbreviate_device(name: &str) -> String {
    name.trim().replace(['\u{2019}', '\u{2018}'], "'").to_uppercase()
}

fn blink(ui: &Ui) -> bool {
    (ui.input(|i| i.time) * 1.25).fract() < 0.6
}

/// One cycle every two seconds, even on and off.
fn slow_blink(ui: &Ui) -> bool {
    (ui.input(|i| i.time) * 0.5).fract() < 0.5
}

/// 199:59, the last value the 3½ digit clock can show.
const LCD_MAX_SECS: u64 = 199 * 60 + 59;

/// Total minutes and seconds in a fixed "1mm:ss" field, like a 3½ digit LCD: the first cell can
/// only show a 1, so 199:59 is the limit (hours roll into the minutes). Unused leading minute
/// digits are `_`, which draws as an unlit cell.
fn lcd_clock(secs: f64) -> String {
    let total = (secs.max(0.0) as u64).min(LCD_MAX_SECS);
    let (m, s) = (total / 60, total % 60);
    if m >= 100 { format!("1{:02}:{s:02}", m - 100) } else { format!("_{m:_>2}:{s:02}") }
}

/// Line 1: what is playing (the constituent track for mixes, in amber).
/// Line 2: context (album, or the mix and its DJ).
/// Line 3: the file's position in the playlist and, inside a mix, the track's position in the setlist.
fn lcd_lines(app: &App) -> (String, Color32, String, String) {
    let Some(entry) = app.player.current_entry() else {
        return ("NULLAMP".into(), theme::LCD_MID, format!("Drop music here, or press {} to add files", widgets::cmd_key('O', false)), String::new());
    };
    let list = &app.player.lists[app.player.playing_list()].playlist;
    let pos_in_list = list.index_of(entry.id).map(|i| format!("FILE {}/{}", i + 1, list.len())).unwrap_or_default();
    let base_title = entry.info.display_line(&entry.path);
    if let Some(setlist) = super::setlist_view::shown(app) {
        let mix_name = setlist.mix.title.clone().unwrap_or_else(|| entry.info.display_title(&entry.path));
        let dj = setlist.mix.dj.clone().or(entry.info.artist.clone());
        let mix_line = match dj {
            Some(dj) if !mix_name.contains(&dj) => format!("MIX ▸ {dj} — {mix_name}"),
            _ => format!("MIX ▸ {mix_name}"),
        };
        if let Some(i) = app.mix.active {
            let e = &setlist.entries[i];
            let approx = if e.timing == Timing::Approximate { "≈ " } else { "" };
            let line1 = format!("{approx}{}", entry_line(e));
            let line3 = format!("{pos_in_list}   TRACK {}/{}", i + 1, setlist.entries.len());
            return (line1, theme::LCD_AMBER, mix_line, line3);
        }
        return (base_title, theme::LCD_FG, mix_line, pos_in_list);
    }
    let line2 = match (&entry.info.album, &entry.info.artist) {
        (Some(album), _) => album.clone(),
        (None, Some(_)) => String::new(),
        (None, None) => entry.path.parent().map(|p| p.display().to_string()).unwrap_or_default(),
    };
    let color = if app.player.status == Status::Stopped { theme::LCD_MID } else { theme::LCD_FG };
    (base_title, color, line2, pos_in_list)
}

/// "Artist — Title", or just the title when the artist is unknown.
pub fn entry_line(e: &nullamp_core::SetlistEntry) -> String {
    if e.artist.trim().is_empty() { e.full_title() } else { format!("{} — {}", e.artist.trim(), e.full_title()) }
}

fn artwork(app: &App, ui: &Ui, rect: Rect) {
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 5.0, theme::LCD_BG);
    let retro = app.settings.retro_artwork && !app.now.show_original_art;
    if let Some(tex) = retro.then_some(app.now.retro_art.as_ref()).flatten().or(app.now.art.as_ref()) {
        let size = tex.size_vec2();
        let scale = (rect.width() / size.x).min(rect.height() / size.y);
        let r = Rect::from_center_size(rect.center(), size * scale);
        // Rounded like the frame, or the picture's square corners poke out past it.
        egui::Image::new(egui::load::SizedTexture::new(tex.id(), r.size())).corner_radius(5.0).paint_at(ui, r);
    } else {
        // Placeholder: a stylised record.
        let c = rect.center();
        p.circle_filled(c, rect.width() * 0.34, Color32::from_rgb(0x10, 0x16, 0x15));
        for r in [0.3, 0.24, 0.18] {
            p.circle_stroke(c, rect.width() * r, Stroke::new(1.0, theme::LCD_OFF));
        }
        p.circle_filled(c, rect.width() * 0.07, theme::ACCENT_DIM);
    }
    p.rect_stroke(rect, 5.0, Stroke::new(1.0, theme::LCD_BORDER), StrokeKind::Inside);
}

fn timeline(app: &mut App, ui: &mut Ui) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click_and_drag());
    let p = ui.painter_at(rect);
    let dur = app.player.duration().filter(|d| *d > 0.0);
    let pos = app.player.position();
    let bar = rect.shrink2(vec2(0.0, 4.0));
    p.rect_filled(bar, 3.0, theme::LCD_BG);

    let x_of = |t: f64, d: f64| bar.left() + (t / d).clamp(0.0, 1.0) as f32 * bar.width();
    let t_of = |x: f32, d: f64| ((x - bar.left()) / bar.width()).clamp(0.0, 1.0) as f64 * d;

    // Setlist spans: the fills go under the waveform, the boundaries over it.
    let spans: Vec<(Rect, bool, Timing)> = match (dur, super::setlist_view::shown(app)) {
        (Some(d), Some(setlist)) => setlist
            .entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                let (s, end) = setlist.span(i, Some(d))?;
                let r = Rect::from_min_max(pos2(x_of(s, d), bar.top()), pos2(x_of(end, d), bar.bottom()));
                Some((r, app.mix.active == Some(i), e.timing))
            })
            .collect(),
        _ => Vec::new(),
    };
    for (i, (r, active, _)) in spans.iter().enumerate() {
        let fill = if *active {
            theme::LCD_AMBER.gamma_multiply(0.22)
        } else if i % 2 == 0 {
            Color32::from_rgb(0x0E, 0x17, 0x15)
        } else {
            Color32::from_rgb(0x0B, 0x12, 0x11)
        };
        p.rect_filled(*r, 0.0, fill);
    }
    if let Some(wave) = &app.now.waveform {
        draw_waveform(&p, bar, wave, ui.ctx().pixels_per_point());
    }
    for (r, active, timing) in &spans {
        let edge = if *timing == Timing::Exact { theme::LCD_MID } else { theme::LCD_MID.gamma_multiply(0.45) };
        if *timing == Timing::Exact {
            p.line_segment([r.left_top(), r.left_bottom()], Stroke::new(1.0, edge));
        } else {
            // Approximate boundaries are dashed: the source only estimated them.
            let mut y = r.top();
            while y < r.bottom() {
                p.line_segment([pos2(r.left(), y), pos2(r.left(), (y + 2.0).min(r.bottom()))], Stroke::new(1.0, edge));
                y += 4.0;
            }
        }
        if *active {
            p.line_segment([r.left_top(), r.right_top()], Stroke::new(2.0, theme::LCD_AMBER));
        }
    }

    // Progress and drag preview.
    let drag_target = if (resp.dragged() || resp.is_pointer_button_down_on()) && dur.is_some() {
        resp.interact_pointer_pos().map(|p| t_of(p.x, dur.unwrap()))
    } else {
        None
    };
    if let Some(d) = dur {
        let shown = drag_target.unwrap_or(pos);
        // Snap once and draw every part from the snapped x. Left alone, epaint snaps rect edges and vertical lines to the
        // pixel grid by different rules and the knob not at all, so the parts step at different moments and drift apart.
        let px = x_of(shown, d).round_to_pixels(ui.ctx().pixels_per_point());
        p.rect_filled(Rect::from_min_max(bar.left_top(), pos2(px, bar.bottom())), 3.0, theme::LCD_FG.gamma_multiply(0.18));
        p.rect_filled(Rect::from_min_max(pos2(px - 1.0, rect.top()), pos2(px + 1.0, rect.bottom())), 0.0, theme::LCD_ACCENT);
        p.circle_filled(pos2(px, rect.top() + 2.0), 3.0, theme::LCD_ACCENT);
    }
    p.rect_stroke(bar, 3.0, Stroke::new(1.0, theme::LCD_BORDER), StrokeKind::Inside);

    // Time labels inside the bar ends.
    p.text(pos2(bar.left() + 6.0, bar.center().y), Align2::LEFT_CENTER, format_clock(drag_target.unwrap_or(pos)), theme::mono(11.0), theme::LCD_TEXT);
    if let Some(d) = dur {
        p.text(pos2(bar.right() - 6.0, bar.center().y), Align2::RIGHT_CENTER, format_clock(d), theme::mono(11.0), theme::LCD_TEXT_DIM);
    }

    if let (Some(d), Some(hover)) = (dur, resp.hover_pos()) {
        let t = t_of(hover.x, d);
        p.line_segment([pos2(hover.x, bar.top()), pos2(hover.x, bar.bottom())], Stroke::new(1.0, Color32::from_white_alpha(60)));
        let track = super::setlist_view::shown(app).and_then(|s| s.active_index(t).map(|i| &s.entries[i])).map(|e| {
            let approx = if e.timing == Timing::Approximate { "≈ " } else { "" };
            format!("{approx}{}", entry_line(e))
        });
        // Shown at once (no hover delay) and to a tenth of a second, so a timestamp can be read off and typed into an entry.
        // A plain area rather than egui::Tooltip: a real tooltip that is always open would make egui show the next widget's
        // tooltip without its usual delay (grace time) when the pointer leaves the timeline.
        egui::Area::new(resp.id.with("hover-time"))
            .order(egui::Order::Tooltip)
            .fixed_pos(hover + vec2(14.0, 20.0))
            .interactable(false)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_max_width(ui.spacing().tooltip_width);
                    ui.label(RichText::new(format_precise(t)).monospace().strong());
                    if let Some(track) = track {
                        ui.label(track);
                    }
                });
            });
    }
    if resp.drag_stopped() || resp.clicked() {
        if let (Some(d), Some(p)) = (dur, resp.interact_pointer_pos().or(resp.hover_pos())) {
            // Land on the tenth of a second the tooltip shows.
            app.player.command(Command::SeekTo((t_of(p.x, d) * 10.0).round() / 10.0), CommandSource::Ui);
        }
    }
}

/// The file's loudness envelope inside `bar`, mirrored around the middle: peaks dim, RMS brighter.
/// One column per point; each takes the loudest of the buckets it covers, so a long mix stays
/// readable and a short track does not turn into a staircase.
fn draw_waveform(p: &egui::Painter, bar: Rect, wave: &nullamp_audio::Waveform, pixels_per_point: f32) {
    let inner = bar.shrink(1.0);
    let columns = inner.width().floor() as usize;
    let n = wave.peak.len().min(wave.rms.len());
    if columns == 0 || n == 0 {
        return;
    }
    let half = inner.height() / 2.0;
    let mid = inner.center().y;
    let width = inner.width() / columns as f32;
    let loudest = |values: &[u8], lo: usize, hi: usize| values[lo..hi].iter().copied().max().unwrap_or(0);
    // The loudest point reaches the edges, so a quietly mastered file is as readable as a loud one. The curve keeps
    // quiet passages visible: loudness is perceived roughly logarithmically.
    let top = f32::from(wave.peak.iter().copied().max().unwrap_or(0).max(1));
    let height = |v: u8| (f32::from(v) / top).powf(0.6) * half;
    let (peak_color, rms_color) = (theme::LCD_FG.gamma_multiply(0.05), theme::LCD_FG.gamma_multiply(0.10));
    let mut mesh = egui::Mesh::default();
    for c in 0..columns {
        let lo = (c * n / columns).min(n - 1);
        let hi = ((c + 1) * n / columns).clamp(lo + 1, n);
        let x0 = (inner.left() + c as f32 * width).round_to_pixels(pixels_per_point);
        let x1 = (inner.left() + (c + 1) as f32 * width).round_to_pixels(pixels_per_point).max(x0 + 1.0 / pixels_per_point);
        for (values, color) in [(&wave.peak, peak_color), (&wave.rms, rms_color)] {
            let h = height(loudest(values, lo, hi)).max(0.5);
            mesh.add_colored_rect(Rect::from_min_max(pos2(x0, mid - h), pos2(x1, mid + h)), color);
        }
    }
    p.add(egui::Shape::mesh(mesh));
}

fn transport(app: &mut App, ui: &mut Ui) {
    let size = vec2(38.0, 28.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let status = app.player.status;
        let run = |app: &mut App, c: Command| app.player.command(c, CommandSource::Ui);
        if widgets::icon_button(ui, Icon::Prev, size, false, "Previous (Z)").clicked() {
            run(app, Command::Previous);
        }
        if widgets::icon_button(ui, Icon::Play, size, status == Status::Playing, "Play (X)").clicked() {
            match status {
                Status::Playing => {}
                Status::Paused => run(app, Command::Play),
                _ => {
                    // Play the selected entry if there is one, else resume / start.
                    if let Some(id) = app.selection.cursor.filter(|_| status == Status::Stopped && app.selection.ids.len() == 1) {
                        let list = app.player.viewing;
                        app.player.play_entry(list, id, 0.0);
                    } else {
                        run(app, Command::Play);
                    }
                }
            }
        }
        if widgets::icon_button(ui, Icon::Pause, size, status == Status::Paused, "Pause (C / Space)").clicked() {
            run(app, Command::PlayPause);
        }
        if widgets::icon_button(ui, Icon::Stop, size, false, "Stop (V)").clicked() {
            run(app, Command::Stop);
        }
        if widgets::icon_button(ui, Icon::Next, size, false, "Next (B)").clicked() {
            run(app, Command::Next);
        }
        ui.add_space(10.0);
        let small = vec2(32.0, 28.0);
        if widgets::icon_button(ui, Icon::Shuffle, small, app.player.nav.shuffle(), "Shuffle (S)").clicked() {
            let on = !app.player.nav.shuffle();
            app.player.set_shuffle(on);
            app.mark_settings_dirty();
        }
        let repeat = app.player.nav.repeat;
        let (icon, tip) = match repeat {
            RepeatMode::One => (Icon::RepeatOne, "Repeat: current track (R)"),
            RepeatMode::All => (Icon::Repeat, "Repeat: playlist (R)"),
            RepeatMode::Off => (Icon::Repeat, "Repeat: off (R)"),
        };
        if widgets::icon_button(ui, icon, small, repeat != RepeatMode::Off, tip).clicked() {
            app.player.set_repeat(repeat.cycle());
            app.mark_settings_dirty();
        }
        ui.add_space(10.0);
        let muted = app.player.muted;
        if widgets::icon_button(ui, if muted { Icon::Muted } else { Icon::Speaker }, small, muted, "Mute (M)").clicked() {
            app.player.command(Command::ToggleMute, CommandSource::Ui);
            app.mark_settings_dirty();
        }
        let mut v = app.player.volume;
        if widgets::level_slider(ui, &mut v, 120.0, muted).changed() {
            app.player.command(Command::SetVolume(v), CommandSource::Ui);
            app.mark_settings_dirty();
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let ins = app.side_open && app.shown_tab() == SideTab::Inspector;
            if widgets::tab(ui, ins, "Inspector").on_hover_text(format!("Metadata inspector ({})", widgets::cmd_key('I', false))).clicked() {
                app.toggle_side(SideTab::Inspector);
            }
            // Single tracks hide it; ⌘L still opens it to add a setlist.
            if app.mix.is_mix {
                let set = app.side_open && app.shown_tab() == SideTab::Setlist;
                let label = match &app.mix.setlist {
                    Some(s) => format!("Setlist · {}", s.entries.len()),
                    None => "Setlist".into(),
                };
                if widgets::tab(ui, set, &label).on_hover_text(format!("DJ mix setlist ({})", widgets::cmd_key('L', false))).clicked() {
                    app.toggle_side(SideTab::Setlist);
                }
            }
            if widgets::tab(ui, app.playlist_open, "Playlist").on_hover_text(format!("Playlist ({})", widgets::cmd_key('P', false))).clicked() {
                app.toggle_playlist();
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::lcd_clock;

    #[test]
    fn lcd_clock_is_a_three_and_a_half_digit_field() {
        assert_eq!(lcd_clock(0.0), "__0:00");
        assert_eq!(lcd_clock(261.0), "__4:21");
        assert_eq!(lcd_clock(45.0 * 60.0), "_45:00");
        assert_eq!(lcd_clock(6000.0), "100:00");
        assert_eq!(lcd_clock(7200.0), "120:00");
        assert_eq!(lcd_clock(f64::MAX), "199:59");
        assert_eq!(lcd_clock(-3.0), "__0:00");
    }
}
