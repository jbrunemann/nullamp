//! The LCD visualiser: a segmented spectrum analyser, an oscilloscope, or nothing.
//!
//! Samples come straight from the audio callback's tap, aligned to what is audible now
//! (see `AudioEngine::scope`), and are analysed every frame on the UI thread. A
//! 1024-point FFT costs microseconds; the short window keeps onsets from smearing.

use std::f32::consts::PI;
use std::time::Duration;

use egui::{Color32, Rect, Sense, Shape, Stroke, Ui, pos2, vec2};

use super::{App, theme};
use crate::player::Status;

const FFT_LEN: usize = 1024;
/// Scope history: one display window plus room to search for a trigger point.
const SCOPE_LEN: usize = 1536;
const SCOPE_WINDOW: usize = 1024;
/// Compensates for the frame being shown a little after it is drawn.
const DISPLAY_LEAD: Duration = Duration::from_millis(12);
const FLOOR_DB: f32 = -64.0;
const LOW_HZ: f32 = 45.0;
const HIGH_HZ: f32 = 16_000.0;
/// Bar fall speed in full heights per second. Rises are instant.
const FALL: f32 = 2.4;
const PEAK_HOLD: f64 = 0.4;
const PEAK_FALL: f32 = 0.9;
const BAR_W: f32 = 3.0;
const BAR_GAP: f32 = 1.0;
const SEG_H: f32 = 2.0;
const SEG_GAP: f32 = 1.0;
const MIN_BRIGHTNESS: f32 = 0.1;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Spectrum,
    Scope,
    Off,
}

impl Mode {
    fn from_setting(s: &str) -> Mode {
        match s {
            "scope" => Mode::Scope,
            "off" => Mode::Off,
            _ => Mode::Spectrum,
        }
    }

    fn setting(self) -> &'static str {
        match self {
            Mode::Spectrum => "spectrum",
            Mode::Scope => "scope",
            Mode::Off => "off",
        }
    }

    fn next(self) -> Mode {
        match self {
            Mode::Spectrum => Mode::Scope,
            Mode::Scope => Mode::Off,
            Mode::Off => Mode::Spectrum,
        }
    }
}

pub struct VizState {
    samples: Vec<f32>,
    window: Vec<f32>,
    re: Vec<f32>,
    im: Vec<f32>,
    twiddles: Vec<(f32, f32)>,
    bars: Vec<f32>,
    peaks: Vec<(f32, f64)>,
    last_time: f64,
}

impl Default for VizState {
    fn default() -> Self {
        let window = (0..FFT_LEN).map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / FFT_LEN as f32).cos()).collect();
        let twiddles = (0..FFT_LEN / 2)
            .map(|k| {
                let a = -2.0 * PI * k as f32 / FFT_LEN as f32;
                (a.cos(), a.sin())
            })
            .collect();
        VizState {
            samples: vec![0.0; SCOPE_LEN.max(FFT_LEN)],
            window,
            re: vec![0.0; FFT_LEN],
            im: vec![0.0; FFT_LEN],
            twiddles,
            bars: Vec::new(),
            peaks: Vec::new(),
            last_time: 0.0,
        }
    }
}

pub fn show(app: &mut App, ui: &mut Ui, rect: Rect) {
    let mode = Mode::from_setting(&app.settings.visualizer);
    let resp = ui.interact(rect, egui::Id::unique("lcd-visualizer"), Sense::click_and_drag());
    if resp.dragged() {
        // A full width of dragging spans the whole brightness range.
        let b = (app.settings.visualizer_brightness + resp.drag_delta().x / rect.width()).clamp(MIN_BRIGHTNESS, 1.0);
        if b != app.settings.visualizer_brightness {
            app.settings.visualizer_brightness = b;
            app.mark_settings_dirty();
        }
    }
    if resp.on_hover_text("Click: spectrum / oscilloscope / off\nDrag left or right: dimmer / brighter").on_hover_cursor(egui::CursorIcon::ResizeHorizontal).clicked() {
        app.settings.visualizer = mode.next().setting().into();
        app.mark_settings_dirty();
    }
    if mode == Mode::Off {
        return;
    }

    let live = matches!(app.player.status, Status::Playing | Status::Paused);
    let viz = &mut app.viz;
    let n = match mode {
        Mode::Spectrum => FFT_LEN,
        Mode::Scope | Mode::Off => SCOPE_LEN,
    };
    let rate = if live { app.player.engine.scope(&mut viz.samples[..n], DISPLAY_LEAD) } else { None };
    if rate.is_none() {
        viz.samples.fill(0.0);
    }

    let time = ui.input(|i| i.time);
    let dt = (time - viz.last_time).clamp(0.0, 0.1) as f32;
    viz.last_time = time;
    let p = ui.painter_at(rect);
    let brightness = app.settings.visualizer_brightness.clamp(MIN_BRIGHTNESS, 1.0);
    let animating = match mode {
        Mode::Spectrum => spectrum(viz, &p, rect, rate.unwrap_or(44_100), time, dt, brightness),
        Mode::Scope => {
            scope(viz, &p, rect, brightness);
            false
        }
        Mode::Off => false,
    };
    if app.player.status == Status::Playing || animating {
        ui.ctx().request_repaint();
    }
}

/// Returns true while bars or peak caps are still settling.
fn spectrum(viz: &mut VizState, p: &egui::Painter, rect: Rect, rate: u32, time: f64, dt: f32, brightness: f32) -> bool {
    let count = ((rect.width() + BAR_GAP) / (BAR_W + BAR_GAP)).floor().max(1.0) as usize;
    if viz.bars.len() != count {
        viz.bars = vec![0.0; count];
        viz.peaks = vec![(0.0, 0.0); count];
    }

    for i in 0..FFT_LEN {
        viz.re[i] = viz.samples[i] * viz.window[i];
        viz.im[i] = 0.0;
    }
    fft(&mut viz.re, &mut viz.im, &viz.twiddles);

    // Full-scale sine = 1.0 after Hann window gain (N/4 per side).
    let norm = 4.0 / FFT_LEN as f32;
    let bin_hz = rate as f32 / FFT_LEN as f32;
    let mag = |k: usize| (viz.re[k] * viz.re[k] + viz.im[k] * viz.im[k]).sqrt() * norm;
    let ratio = (HIGH_HZ / LOW_HZ).powf(1.0 / count as f32);
    let mut animating = false;
    for b in 0..count {
        let f0 = LOW_HZ * ratio.powi(b as i32);
        let f1 = f0 * ratio;
        let (k0, k1) = (f0 / bin_hz, f1 / bin_hz);
        let m = if k1 - k0 < 1.5 {
            // Narrower than a bin (the bass end): interpolate at the band centre so
            // neighbouring bars do not show identical steps.
            let k = ((k0 + k1) / 2.0).clamp(1.0, (FFT_LEN / 2 - 2) as f32);
            let (i, f) = (k.floor() as usize, k.fract());
            mag(i) * (1.0 - f) + mag(i + 1) * f
        } else {
            (k0.ceil() as usize..=(k1.floor() as usize).min(FFT_LEN / 2 - 1)).map(mag).fold(0.0, f32::max)
        };
        // Music falls off about 3 dB per octave; tilt it back so the highs move too.
        let tilt = 3.0 * ((f0 * f1).sqrt() / 1000.0).log2();
        let db = 20.0 * m.max(1e-9).log10() + tilt;
        let target = ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0);

        let bar = &mut viz.bars[b];
        *bar = if target >= *bar { target } else { (*bar - FALL * dt).max(target) };
        let peak = &mut viz.peaks[b];
        if *bar >= peak.0 {
            *peak = (*bar, time + PEAK_HOLD);
        } else if time > peak.1 {
            peak.0 = (peak.0 - PEAK_FALL * dt).max(*bar);
        }
        animating |= *bar > 0.0 || peak.0 > 0.0;
    }

    let segs = ((rect.height() + SEG_GAP) / (SEG_H + SEG_GAP)).floor().max(1.0) as usize;
    let mut shapes = Vec::with_capacity(count * segs);
    for b in 0..count {
        let x = rect.left() + b as f32 * (BAR_W + BAR_GAP);
        let lit = (viz.bars[b] * segs as f32).round() as usize;
        let peak = ((viz.peaks[b].0 * segs as f32).round() as usize).min(segs);
        for s in 0..segs {
            let y = rect.bottom() - (s + 1) as f32 * (SEG_H + SEG_GAP) + SEG_GAP;
            let color = if s < lit {
                dim(lerp(theme::LCD_MID, theme::LCD_FG, s as f32 / segs as f32), brightness)
            } else if peak > 0 && s + 1 == peak {
                dim(theme::LCD_AMBER, brightness)
            } else {
                theme::LCD_OFF
            };
            shapes.push(Shape::rect_filled(Rect::from_min_size(pos2(x, y), vec2(BAR_W, SEG_H)), 0.0, color));
        }
    }
    p.extend(shapes);
    animating
}

fn scope(viz: &VizState, p: &egui::Painter, rect: Rect, brightness: f32) {
    let s = &viz.samples[..SCOPE_LEN];
    // Trigger on the latest rising zero crossing that still leaves a full window, so a
    // steady tone stands still instead of rolling.
    let latest = SCOPE_LEN - SCOPE_WINDOW;
    let start = (1..=latest).rev().find(|&i| s[i - 1] < 0.0 && s[i] >= 0.0).unwrap_or(latest);
    let w = &s[start..start + SCOPE_WINDOW];

    let mid = rect.center().y;
    p.line_segment([pos2(rect.left(), mid), pos2(rect.right(), mid)], Stroke::new(1.0, theme::LCD_OFF));
    let cols = rect.width().max(2.0) as usize;
    let amp = rect.height() / 2.0 - 1.0;
    let points: Vec<_> = (0..cols)
        .map(|c| {
            let v = w[c * (SCOPE_WINDOW - 1) / (cols - 1)].clamp(-1.0, 1.0);
            pos2(rect.left() + c as f32, mid - v * amp)
        })
        .collect();
    // A wide faint pass under the trace reads as phosphor glow.
    let trace = dim(theme::LCD_FG, brightness);
    p.add(Shape::line(points.clone(), Stroke::new(3.0, trace.gamma_multiply(0.18))));
    p.add(Shape::line(points, Stroke::new(1.2, trace)));
}

/// A lit colour at reduced brightness: fades towards the unlit LCD segment.
fn dim(c: Color32, brightness: f32) -> Color32 {
    lerp(theme::LCD_OFF, c, brightness)
}

fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

/// In-place iterative radix-2 FFT. `re.len()` must be a power of two and
/// `twiddles[k] = e^(-2πik/N)` for k < N/2.
fn fft(re: &mut [f32], im: &mut [f32], twiddles: &[(f32, f32)]) {
    let n = re.len();
    let bits = n.trailing_zeros();
    for i in 0..n {
        let j = i.reverse_bits() >> (usize::BITS - bits);
        if j > i {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let step = n / len;
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (wr, wi) = twiddles[k * step];
                let (a, b) = (start + k, start + k + len / 2);
                let tr = re[b] * wr - im[b] * wi;
                let ti = re[b] * wi + im[b] * wr;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
        }
        len *= 2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_cycles_spectrum_scope_off_and_back() {
        let mut m = Mode::Spectrum;
        let seen: Vec<_> = (0..3).map(|_| { m = m.next(); m.setting() }).collect();
        assert_eq!(seen, ["scope", "off", "spectrum"]);
        assert!(["spectrum", "scope", "off"].iter().all(|s| Mode::from_setting(s).setting() == *s));
    }

    #[test]
    fn fft_finds_a_full_scale_sine_at_its_bin() {
        let v = VizState::default();
        let (mut re, mut im) = (vec![0.0; FFT_LEN], vec![0.0; FFT_LEN]);
        for (i, (r, w)) in re.iter_mut().zip(&v.window).enumerate() {
            *r = (2.0 * PI * 64.0 * i as f32 / FFT_LEN as f32).sin() * w;
        }
        fft(&mut re, &mut im, &v.twiddles);
        let mag: Vec<f32> = (0..FFT_LEN / 2).map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt() * 4.0 / FFT_LEN as f32).collect();
        let peak = (0..mag.len()).max_by(|a, b| mag[*a].total_cmp(&mag[*b])).unwrap();
        assert_eq!(peak, 64);
        assert!((mag[64] - 1.0).abs() < 0.01, "{}", mag[64]);
        assert!(mag[80] < 1e-3);
    }
}
