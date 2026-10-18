//! The deck: LCD display, artwork, mix timeline and transport controls.

use egui::emath::GuiRounding;
use egui::{Align2, Color32, Rect, RichText, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
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
        if art_resp.on_hover_text("Artwork · click to open the inspector").clicked() {
            app.toggle_side(SideTab::Inspector);
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
    let time_text = match (app.show_remaining, dur) {
        (true, Some(d)) => format!("-{}", lcd_clock(d - pos)),
        _ => format!(" {}", lcd_clock(pos)),
    };
    let time_origin = pos2(inner.left() + 16.0, inner.top() + 2.0);
    let w = widgets::seven_segment_width("-0:00:00", digits_h);
    // Unlit "8"s behind the digits for the LCD look.
    let ghost: String = time_text.chars().map(|c| if c.is_ascii_digit() { '8' } else { c }).collect();
    widgets::seven_segment(&p, time_origin, digits_h, &ghost, theme::LCD_OFF, Color32::TRANSPARENT);
    let lit = if status == Status::Stopped { theme::LCD_MID } else { theme::LCD_FG };
    widgets::seven_segment(&p, time_origin, digits_h, &time_text, lit, Color32::TRANSPARENT);
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
    widgets::marquee(ui, l1, &line1, theme::mono(16.0), line1_color, time);
    let l2 = Rect::from_min_size(pos2(text_rect.left(), l1.bottom() + 5.0), vec2(text_rect.width(), 16.0));
    widgets::marquee(ui, l2, &line2, theme::mono(12.0), theme::LCD_MID, time);
    p.text(pos2(text_rect.left(), l2.bottom() + 8.0), Align2::LEFT_TOP, line3, theme::mono(10.0), theme::LCD_MID);

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

fn blink(ui: &Ui) -> bool {
    (ui.input(|i| i.time) * 1.25).fract() < 0.6
}

fn lcd_clock(secs: f64) -> String {
    let s = format_clock(secs.max(0.0));
    // Right-align in an "h:mm:ss" field so digits do not jump.
    format!("{s:>7}")
}

/// Line 1: what is playing (the constituent track for mixes, in amber).
/// Line 2: context (album, or the mix and its DJ).
/// Line 3: the file's position in the playlist and, inside a mix, the track's position in the setlist.
fn lcd_lines(app: &App) -> (String, Color32, String, String) {
    let Some(entry) = app.player.current_entry() else {
        return ("NULLAMP".into(), theme::LCD_MID, "Drop music here, or press ⌘O / Ctrl+O to add files".into(), String::new());
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
    // While a mix plays, show the constituent track's artwork when it has one.
    let tex = super::setlist_view::active_entry_art(app).or(app.now.art.as_ref());
    if let Some(tex) = tex {
        let size = tex.size_vec2();
        let scale = (rect.width() / size.x).min(rect.height() / size.y);
        let r = Rect::from_center_size(rect.center(), size * scale);
        p.image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
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

    // Setlist spans.
    if let (Some(d), Some(setlist)) = (dur, super::setlist_view::shown(app)) {
        for (i, e) in setlist.entries.iter().enumerate() {
            let Some((s, end)) = setlist.span(i, Some(d)) else { continue };
            let r = Rect::from_min_max(pos2(x_of(s, d), bar.top()), pos2(x_of(end, d), bar.bottom()));
            let active = app.mix.active == Some(i);
            let fill = if active {
                theme::LCD_AMBER.gamma_multiply(0.22)
            } else if i % 2 == 0 {
                Color32::from_rgb(0x0E, 0x17, 0x15)
            } else {
                Color32::from_rgb(0x0B, 0x12, 0x11)
            };
            p.rect_filled(r, 0.0, fill);
            let edge = if e.timing == Timing::Exact { theme::LCD_MID } else { theme::LCD_MID.gamma_multiply(0.45) };
            if e.timing == Timing::Exact {
                p.line_segment([r.left_top(), r.left_bottom()], Stroke::new(1.0, edge));
            } else {
                // Approximate boundaries are dashed: the source only estimated them.
                let mut y = r.top();
                while y < r.bottom() {
                    p.line_segment([pos2(r.left(), y), pos2(r.left(), (y + 2.0).min(r.bottom()))], Stroke::new(1.0, edge));
                    y += 4.0;
                }
            }
            if active {
                p.line_segment([r.left_top(), r.right_top()], Stroke::new(2.0, theme::LCD_AMBER));
            }
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
    p.text(pos2(bar.left() + 6.0, bar.center().y), Align2::LEFT_CENTER, format_clock(drag_target.unwrap_or(pos)), theme::mono(10.5), theme::LCD_TEXT);
    if let Some(d) = dur {
        p.text(pos2(bar.right() - 6.0, bar.center().y), Align2::RIGHT_CENTER, format_clock(d), theme::mono(10.5), theme::LCD_TEXT_DIM);
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
            if widgets::tab(ui, ins, "Inspector").on_hover_text("Metadata inspector (⌘I / Ctrl+I)").clicked() {
                app.toggle_side(SideTab::Inspector);
            }
            // Single tracks hide it; ⌘L still opens it to add a setlist.
            if app.mix.is_mix {
                let set = app.side_open && app.shown_tab() == SideTab::Setlist;
                let label = match &app.mix.setlist {
                    Some(s) => format!("Setlist · {}", s.entries.len()),
                    None => "Setlist".into(),
                };
                if widgets::tab(ui, set, &label).on_hover_text("DJ mix setlist (⌘L / Ctrl+L)").clicked() {
                    app.toggle_side(SideTab::Setlist);
                }
            }
            if widgets::tab(ui, app.playlist_open, "Playlist").on_hover_text("Playlist (⌘P / Ctrl+P)").clicked() {
                app.toggle_playlist();
            }
        });
    });
}
