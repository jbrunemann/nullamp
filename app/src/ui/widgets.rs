//! Custom-painted widgets: seven-segment digits, transport icons, buttons.

use egui::{Color32, CornerRadius, Painter, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};

use super::theme;

// ----- Seven-segment display -----

const SEGMENTS: [u8; 10] = [
    0b011_1111, // 0: a b c d e f
    0b000_0110, // 1: b c
    0b101_1011, // 2: a b d e g
    0b100_1111, // 3: a b c d g
    0b110_0110, // 4: b c f g
    0b110_1101, // 5: a c d f g
    0b111_1101, // 6: a c d e f g
    0b000_0111, // 7: a b c
    0b111_1111, // 8
    0b110_1111, // 9: a b c d f g
];

/// Width of `text` when drawn with [`seven_segment`] at `height`.
pub fn seven_segment_width(text: &str, height: f32) -> f32 {
    text.chars().map(|c| char_advance(c, height)).sum()
}

fn char_advance(c: char, h: f32) -> f32 {
    match c {
        ':' | '.' => h * 0.28,
        ' ' => h * 0.3,
        _ => h * 0.62,
    }
}

/// Draw digits, '-', ':' and ' ' as slanted seven-segment glyphs. Unlit segments
/// are drawn in `off` for the LCD look. Returns the width used.
pub fn seven_segment(painter: &Painter, origin: Pos2, height: f32, text: &str, on: Color32, off: Color32) -> f32 {
    let mut x = origin.x;
    for c in text.chars() {
        let adv = char_advance(c, height);
        match c {
            '0'..='9' => digit(painter, pos2(x, origin.y), height, SEGMENTS[c as usize - '0' as usize], on, off),
            '-' => digit(painter, pos2(x, origin.y), height, 0b100_0000, on, off),
            ':' => {
                let r = height * 0.06;
                for fy in [0.32, 0.72] {
                    let p = pos2(x + adv * 0.45 + slant(height, fy), origin.y + height * fy);
                    painter.circle_filled(p, r, on);
                }
            }
            '.' => {
                let r = height * 0.06;
                painter.circle_filled(pos2(x + adv * 0.4, origin.y + height * 0.94), r, on);
            }
            _ => {}
        }
        x += adv;
    }
    x - origin.x
}

/// Horizontal offset for italic slant at a fraction `fy` of the glyph height.
fn slant(h: f32, fy: f32) -> f32 {
    (0.5 - fy) * h * 0.12
}

fn digit(painter: &Painter, o: Pos2, h: f32, mask: u8, on: Color32, off: Color32) {
    let w = h * 0.5;
    let t = h * 0.11;
    let g = t * 0.25; // gap between segment tips
    // Segment centre lines: verticals at the left/right edges, horizontals at
    // top/middle/bottom, all inset by half a stroke so the glyph is symmetric.
    let (xl, xr) = (t * 0.5, w - t * 0.5);
    let (yt, ym, yb) = (t * 0.5, h * 0.5, h - t * 0.5);
    // (is_horizontal, start, end) along the centre line.
    let segs: [(bool, Pos2, Pos2); 7] = [
        (true, pos2(xl, yt), pos2(xr, yt)),   // a
        (false, pos2(xr, yt), pos2(xr, ym)),  // b
        (false, pos2(xr, ym), pos2(xr, yb)),  // c
        (true, pos2(xl, yb), pos2(xr, yb)),   // d
        (false, pos2(xl, ym), pos2(xl, yb)),  // e
        (false, pos2(xl, yt), pos2(xl, ym)),  // f
        (true, pos2(xl, ym), pos2(xr, ym)),   // g
    ];
    let half = t * 0.5;
    for (i, (horizontal, a, b)) in segs.iter().enumerate() {
        let color = if mask & (1 << i) != 0 { on } else { off };
        if color == Color32::TRANSPARENT {
            continue;
        }
        // Hexagon with pointed tips, shortened by the gap at both ends.
        let pts: Vec<Pos2> = if *horizontal {
            let (x0, x1, y) = (a.x + g, b.x - g, a.y);
            vec![pos2(x0, y), pos2(x0 + half, y - half), pos2(x1 - half, y - half), pos2(x1, y), pos2(x1 - half, y + half), pos2(x0 + half, y + half)]
        } else {
            let (y0, y1, x) = (a.y + g, b.y - g, a.x);
            vec![pos2(x, y0), pos2(x + half, y0 + half), pos2(x + half, y1 - half), pos2(x, y1), pos2(x - half, y1 - half), pos2(x - half, y0 + half)]
        };
        let pts = pts.into_iter().map(|p| pos2(o.x + p.x + slant(h, p.y / h), o.y + p.y)).collect();
        painter.add(Shape::convex_polygon(pts, color, Stroke::NONE));
    }
}

// ----- Icons -----

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Prev,
    Play,
    Pause,
    Stop,
    Next,
    Shuffle,
    Repeat,
    RepeatOne,
    Speaker,
    Muted,
}

pub fn paint_icon(painter: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let c = rect.center();
    let s = rect.width().min(rect.height()) * 0.5;
    let tri = |painter: &Painter, x0: f32, x1: f32| {
        painter.add(Shape::convex_polygon(vec![pos2(x0, c.y - s * 0.6), pos2(x1, c.y), pos2(x0, c.y + s * 0.6)], color, Stroke::NONE));
    };
    let stroke = Stroke::new((s * 0.16).max(1.4), color);
    match icon {
        Icon::Play => tri(painter, c.x - s * 0.45, c.x + s * 0.6),
        Icon::Pause => {
            for dx in [-0.32, 0.32] {
                painter.rect_filled(Rect::from_center_size(pos2(c.x + dx * s, c.y), vec2(s * 0.28, s * 1.2)), 1.0, color);
            }
        }
        Icon::Stop => {
            painter.rect_filled(Rect::from_center_size(c, Vec2::splat(s * 1.1)), 0.0, color);
        }
        Icon::Prev => {
            painter.rect_filled(Rect::from_center_size(pos2(c.x - s * 0.55, c.y), vec2(s * 0.18, s * 1.2)), 0.5, color);
            painter.add(Shape::convex_polygon(
                vec![pos2(c.x + s * 0.55, c.y - s * 0.6), pos2(c.x - s * 0.4, c.y), pos2(c.x + s * 0.55, c.y + s * 0.6)],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Next => {
            tri(painter, c.x - s * 0.55, c.x + s * 0.4);
            painter.rect_filled(Rect::from_center_size(pos2(c.x + s * 0.55, c.y), vec2(s * 0.18, s * 1.2)), 0.5, color);
        }
        Icon::Shuffle => {
            let (l, r, t, b) = (c.x - s * 0.75, c.x + s * 0.55, c.y - s * 0.45, c.y + s * 0.45);
            painter.line_segment([pos2(l, b), pos2(r, t)], stroke);
            painter.line_segment([pos2(l, t), pos2(c.x - s * 0.15, c.y - s * 0.05)], stroke);
            painter.line_segment([pos2(c.x + s * 0.1, c.y + s * 0.15), pos2(r, b)], stroke);
            for y in [t, b] {
                let head = s * 0.28;
                painter.add(Shape::convex_polygon(
                    vec![pos2(r + head * 0.9, y), pos2(r - head * 0.2, y - head * 0.75), pos2(r - head * 0.2, y + head * 0.75)],
                    color,
                    Stroke::NONE,
                ));
            }
        }
        Icon::Repeat | Icon::RepeatOne => {
            let r = Rect::from_center_size(c, vec2(s * 1.5, s * 0.95));
            painter.rect_stroke(r, CornerRadius::same((s * 0.35) as u8), stroke, StrokeKind::Middle);
            let head = s * 0.3;
            let p = pos2(r.center().x + s * 0.1, r.top());
            painter.add(Shape::convex_polygon(
                vec![pos2(p.x + head, p.y), pos2(p.x - head * 0.2, p.y - head * 0.8), pos2(p.x - head * 0.2, p.y + head * 0.8)],
                color,
                Stroke::NONE,
            ));
            if icon == Icon::RepeatOne {
                painter.text(c, egui::Align2::CENTER_CENTER, "1", theme::prop(s * 0.8), color);
            }
        }
        Icon::Speaker | Icon::Muted => {
            let body = vec![
                pos2(c.x - s * 0.75, c.y - s * 0.25),
                pos2(c.x - s * 0.4, c.y - s * 0.25),
                pos2(c.x, c.y - s * 0.6),
                pos2(c.x, c.y + s * 0.6),
                pos2(c.x - s * 0.4, c.y + s * 0.25),
                pos2(c.x - s * 0.75, c.y + s * 0.25),
            ];
            painter.add(Shape::convex_polygon(body, color, Stroke::NONE));
            if icon == Icon::Muted {
                painter.line_segment([pos2(c.x + s * 0.25, c.y - s * 0.3), pos2(c.x + s * 0.8, c.y + s * 0.3)], stroke);
                painter.line_segment([pos2(c.x + s * 0.25, c.y + s * 0.3), pos2(c.x + s * 0.8, c.y - s * 0.3)], stroke);
            } else {
                for k in [0.35, 0.65] {
                    let pts: Vec<Pos2> = (0..=8)
                        .map(|i| {
                            let a = -0.8 + 1.6 * i as f32 / 8.0;
                            pos2(c.x + s * 0.1 + s * k * a.cos(), c.y + s * k * a.sin())
                        })
                        .collect();
                    painter.add(Shape::line(pts, Stroke::new(stroke.width * 0.8, color)));
                }
            }
        }
    }
}

/// A raised, tactile button with a painted icon. `lit` shows an active state.
pub fn icon_button(ui: &mut Ui, icon: Icon, size: Vec2, lit: bool, tooltip: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let pressed = resp.is_pointer_button_down_on();
        let fill = if pressed {
            theme::bg()
        } else if resp.hovered() {
            theme::raised_hover()
        } else {
            theme::raised()
        };
        let p = ui.painter();
        p.rect_filled(rect, 4.0, fill);
        // Bevel: light top edge, dark bottom edge.
        if !pressed {
            p.line_segment([rect.left_top() + vec2(3.0, 0.5), rect.right_top() + vec2(-3.0, 0.5)], Stroke::new(1.0, if theme::is_light() { Color32::WHITE } else { Color32::from_white_alpha(18) }));
            p.line_segment([rect.left_bottom() + vec2(3.0, -0.5), rect.right_bottom() + vec2(-3.0, -0.5)], Stroke::new(1.0, Color32::from_black_alpha(if theme::is_light() { 28 } else { 90 })));
        }
        p.rect_stroke(rect, 4.0, Stroke::new(1.0, if resp.has_focus() { theme::select_stroke() } else { theme::border() }), StrokeKind::Inside);
        let color = if lit { theme::accent() } else if resp.hovered() { theme::text_strong() } else { theme::text() };
        let inner = rect.shrink(rect.height() * 0.24);
        paint_icon(p, inner.translate(if pressed { vec2(0.0, 1.0) } else { Vec2::ZERO }), icon, color);
        if lit {
            let led = Rect::from_center_size(pos2(rect.center().x, rect.bottom() - 3.0), vec2(rect.width() * 0.35, 2.0));
            p.rect_filled(led, 1.0, theme::accent());
        }
    }
    resp.on_hover_text(tooltip)
}

/// Text that scrolls horizontally when it does not fit, like a hardware display.
pub fn marquee(ui: &Ui, rect: Rect, text: &str, font: egui::FontId, color: Color32, time: f64) {
    let painter = ui.painter_at(rect);
    let galley = painter.layout_no_wrap(text.to_owned(), font, color);
    let w = galley.size().x;
    let y = rect.center().y - galley.size().y / 2.0;
    if w <= rect.width() {
        painter.galley(pos2(rect.left(), y), galley, color);
        return;
    }
    let gap = 48.0;
    let speed = 28.0; // px per second
    let cycle = w + gap;
    // Pause briefly at the start of each cycle so the beginning is readable.
    let hold = 1.5;
    let period = hold + cycle as f64 / speed;
    let t = time % period;
    let offset = if t < hold { 0.0 } else { ((t - hold) * speed) as f32 };
    let x = rect.left() - offset;
    painter.galley(pos2(x, y), galley.clone(), color);
    painter.galley(pos2(x + cycle, y), galley, color);
}

/// Small pill-shaped toggle for panel tabs and filters.
pub fn tab(ui: &mut Ui, selected: bool, label: &str) -> Response {
    let text = egui::RichText::new(label).size(12.0).color(if selected { theme::text() } else { theme::text_dim() });
    let resp = ui.add(egui::Button::new(text).selected(selected).corner_radius(3.0).min_size(vec2(0.0, 22.0)));
    if selected {
        let r = resp.rect;
        ui.painter().line_segment([pos2(r.left() + 4.0, r.bottom() - 1.0), pos2(r.right() - 4.0, r.bottom() - 1.0)], Stroke::new(2.0, theme::accent()));
    }
    resp
}

/// A horizontal level control (volume) drawn as a segmented meter.
pub fn level_slider(ui: &mut Ui, value: &mut f32, width: f32, muted: bool) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(vec2(width, 18.0), Sense::click_and_drag());
    if let Some(p) = resp.interact_pointer_pos() {
        let v = ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        if (v - *value).abs() > f32::EPSILON {
            *value = v;
            resp.mark_changed();
        }
    }
    if resp.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y + i.smooth_scroll_delta.x);
        if scroll != 0.0 {
            *value = (*value + scroll * 0.002).clamp(0.0, 1.0);
            resp.mark_changed();
        }
    }
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        p.rect_filled(rect, 3.0, theme::LCD_BG);
        let n = 20;
        let seg_w = (rect.width() - 4.0) / n as f32;
        for i in 0..n {
            let x = rect.left() + 2.0 + i as f32 * seg_w;
            let h = 4.0 + (rect.height() - 8.0) * (i as f32 + 1.0) / n as f32;
            let seg = Rect::from_min_max(pos2(x + 0.5, rect.bottom() - 2.0 - h), pos2(x + seg_w - 1.0, rect.bottom() - 2.0));
            let lit = (i as f32 + 0.5) / n as f32 <= *value;
            let color = match (lit, muted) {
                (true, true) => theme::LCD_MID.gamma_multiply(0.5),
                (true, false) if i >= n - 3 => theme::LCD_AMBER,
                (true, false) => theme::LCD_FG,
                (false, _) => theme::LCD_OFF,
            };
            p.rect_filled(seg, 0.5, color);
        }
    }
    resp.on_hover_text(format!("Volume {:.0}%  (scroll to adjust)", *value * 100.0))
}

/// A small picture of how an audio output connects. Real hardware is drawn solid;
/// virtual devices are drawn as a dashed outline.
pub fn paint_output_icon(painter: &Painter, rect: Rect, kind: nullamp_audio::OutputKind, color: Color32) {
    use nullamp_audio::OutputKind;
    let c = rect.center();
    let s = rect.width().min(rect.height()) * 0.5;
    let stroke = Stroke::new((s * 0.17).max(1.2), color);
    let outline = |painter: &Painter, r: Rect| painter.rect_stroke(r, 1.0, stroke, StrokeKind::Middle);
    match kind {
        OutputKind::BuiltIn => {
            // A laptop: screen above a wider base.
            outline(painter, Rect::from_center_size(pos2(c.x, c.y - s * 0.15), vec2(s * 1.3, s * 0.9)));
            painter.line_segment([pos2(c.x - s * 0.95, c.y + s * 0.6), pos2(c.x + s * 0.95, c.y + s * 0.6)], Stroke::new(stroke.width * 1.3, color));
        }
        OutputKind::Usb => {
            // A plug: connector with two pins, and the cable.
            painter.rect_filled(Rect::from_center_size(pos2(c.x, c.y - s * 0.1), vec2(s * 1.0, s * 0.6)), 1.0, color);
            for dx in [-0.25, 0.25] {
                painter.rect_filled(Rect::from_center_size(pos2(c.x + dx * s, c.y - s * 0.6), vec2(s * 0.2, s * 0.3)), 0.0, color);
            }
            painter.line_segment([pos2(c.x, c.y + s * 0.2), pos2(c.x, c.y + s * 0.85)], stroke);
        }
        OutputKind::Bluetooth => {
            let pts = [(-0.45, -0.35), (0.4, 0.4), (0.0, 0.8), (0.0, -0.8), (0.4, -0.4), (-0.45, 0.35)];
            painter.add(Shape::line(pts.iter().map(|(x, y)| pos2(c.x + x * s, c.y + y * s)).collect(), stroke));
        }
        OutputKind::Display => {
            // A monitor on a stand.
            outline(painter, Rect::from_center_size(pos2(c.x, c.y - s * 0.2), vec2(s * 1.6, s * 1.0)));
            painter.line_segment([pos2(c.x, c.y + s * 0.3), pos2(c.x, c.y + s * 0.6)], stroke);
            painter.line_segment([pos2(c.x - s * 0.45, c.y + s * 0.65), pos2(c.x + s * 0.45, c.y + s * 0.65)], stroke);
        }
        OutputKind::Network => {
            // Radiating arcs over a dot.
            let o = pos2(c.x, c.y + s * 0.55);
            painter.circle_filled(o, s * 0.14, color);
            for k in [0.55, 1.0, 1.45] {
                let pts: Vec<Pos2> = (0..=8)
                    .map(|i| {
                        let a = -std::f32::consts::FRAC_PI_2 - 0.85 + 1.7 * i as f32 / 8.0;
                        pos2(o.x + s * k * a.cos(), o.y + s * k * a.sin())
                    })
                    .collect();
                painter.add(Shape::line(pts, Stroke::new(stroke.width * 0.9, color)));
            }
        }
        OutputKind::Card => {
            // A sound card: board with its gold contacts along the bottom edge.
            let board = Rect::from_center_size(pos2(c.x, c.y - s * 0.15), vec2(s * 1.6, s * 1.0));
            outline(painter, board);
            for i in 0..4 {
                let x = board.left() + s * 0.2 + i as f32 * s * 0.4;
                painter.line_segment([pos2(x, board.bottom() + 1.0), pos2(x, board.bottom() + s * 0.35)], stroke);
            }
        }
        OutputKind::Virtual => {
            let r = Rect::from_center_size(c, Vec2::splat(s * 1.4));
            let pts = vec![r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
            painter.extend(Shape::dashed_line(&pts, stroke, s * 0.35, s * 0.3));
        }
        OutputKind::Unknown => paint_icon(painter, rect, Icon::Speaker, color),
    }
}
