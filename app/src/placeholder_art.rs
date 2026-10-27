//! The cover of a mix track that no service had a cover for: a retro scene (a landscape with a
//! perspective grid, a skyline, a neon city on a grid, dunes or deep space) with one of several
//! suns, drawn from the track's name.
//!
//! The picture is a pure function of the `entry_art::key`, so a track looks the same in every
//! mix and on every run, and nothing needs caching. The seed is FNV-1a, not std's hasher, which
//! may change between Rust releases.

use crate::workers::Thumb;

/// Same side as the other covers (`decode_thumb(.., 512)`).
pub const SIZE: usize = 512;
const PX: f32 = SIZE as f32;

type Rgb = [f32; 3];

/// SplitMix64: small, and every seed gives a good sequence.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// In `[0, 1)`.
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }

    fn between(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }
}

fn hsv(h: f32, s: f32, v: f32) -> Rgb {
    let h = h.rem_euclid(1.0) * 6.0;
    let (i, f) = (h.floor(), h.fract());
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    match i as u32 {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t)
}

fn smooth(lo: f32, hi: f32, x: f32) -> f32 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Coverage of a pixel from its signed distance to a shape's edge in pixels (negative inside).
fn coverage(distance: f32) -> f32 {
    (0.5 - distance).clamp(0.0, 1.0)
}

struct Canvas {
    pixels: Vec<Rgb>,
}

impl Canvas {
    fn blend(&mut self, x: usize, y: usize, color: Rgb, alpha: f32) {
        if alpha > 0.0 {
            let p = &mut self.pixels[y * SIZE + x];
            *p = mix(*p, color, alpha.min(1.0));
        }
    }

    /// Calls `shade(x, y)` with unit coordinates for the pixels of the box around (`cx`, `cy`)
    /// that is `rx` by `ry` units wide and high, plus a margin for anti-aliasing. It returns a
    /// colour and an alpha.
    fn paint_box(&mut self, cx: f32, cy: f32, rx: f32, ry: f32, shade: impl Fn(f32, f32) -> (Rgb, f32)) {
        let range = |c: f32, r: f32| ((((c - r) * PX) as isize - 3).max(0) as usize, (((c + r) * PX) as isize + 3).clamp(0, SIZE as isize) as usize);
        let ((x0, x1), (y0, y1)) = (range(cx, rx), range(cy, ry));
        for y in y0..y1 {
            for x in x0..x1 {
                let (color, alpha) = shade((x as f32 + 0.5) / PX, (y as f32 + 0.5) / PX);
                self.blend(x, y, color, alpha);
            }
        }
    }

    fn disc(&mut self, cx: f32, cy: f32, radius: f32, color: Rgb, alpha: f32) {
        self.paint_box(cx, cy, radius, radius, |x, y| (color, coverage((x - cx).hypot(y - cy) * PX - radius * PX) * alpha));
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Scene {
    Landscape,
    Skyline,
    Tron,
    Space,
    Dunes,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Sun {
    Striped,
    Outline,
    Twin,
    Eclipse,
    None,
}

/// One of `items`, chosen by weight.
fn pick<T: Copy>(rng: &mut Rng, items: &[(T, u32)]) -> T {
    let total: u32 = items.iter().map(|(_, w)| w).sum();
    let mut roll = (rng.next() % u64::from(total)) as u32;
    for &(item, weight) in items {
        if roll < weight {
            return item;
        }
        roll -= weight;
    }
    items[0].0
}

/// The seed's first draws: the hue, the kind of scene and the style of the sun.
fn start(key: &str) -> (Rng, f32, Scene, Sun) {
    let mut rng = Rng(crate::store::fnv1a(key.as_bytes()));
    let hue = rng.unit();
    let scene = pick(&mut rng, &[(Scene::Landscape, 2), (Scene::Skyline, 3), (Scene::Tron, 4), (Scene::Space, 2), (Scene::Dunes, 2)]);
    let sun = pick(&mut rng, &[(Sun::Striped, 4), (Sun::Outline, 2), (Sun::Twin, 2), (Sun::Eclipse, 2), (Sun::None, 1)]);
    (rng, hue, scene, sun)
}

/// The colours the layers of a scene share, all derived from its hue.
struct Tones {
    hue: f32,
    sun_top: Rgb,
    sun_bottom: Rgb,
    ground: Rgb,
    glow: Rgb,
}

/// The cover for the track with this `entry_art::key`.
pub fn generate(key: &str) -> Thumb {
    compose(key, None, None)
}

fn compose(key: &str, scene: Option<Scene>, sun: Option<Sun>) -> Thumb {
    let (mut rng, hue, drawn_scene, drawn_sun) = start(key);
    let (scene, sun_style) = (scene.unwrap_or(drawn_scene), sun.unwrap_or(drawn_sun));
    let horizon = match scene {
        Scene::Landscape => rng.between(0.35, 0.8),
        Scene::Skyline => rng.between(0.6, 0.82),
        Scene::Tron => rng.between(0.55, 0.72),
        Scene::Dunes => rng.between(0.45, 0.7),
        // No ground: the sky runs to the bottom edge.
        Scene::Space => 1.0,
    };
    // The neon city keeps to the colours of the films: cyan, orange or red, never the rest.
    let tron = scene == Scene::Tron;
    let hue = if tron { pick(&mut rng, &[(0.52, 4), (0.07, 3), (0.97, 1)]) } else { hue };
    let tones = if tron {
        Tones { hue, sun_top: hsv(hue, 0.6, 1.0), sun_bottom: hsv(hue + 0.05, 0.9, 0.9), ground: hsv(hue, 0.9, 0.02), glow: hsv(hue, 0.7, 1.0) }
    } else {
        Tones { hue, sun_top: hsv(hue + 0.12, 0.85, 1.0), sun_bottom: hsv(hue + 0.32, 0.9, 1.0), ground: hsv(hue + 0.5, 0.6, 0.1), glow: hsv(hue + 0.5, 0.8, 1.0) }
    };

    // Sky.
    let space = scene == Scene::Space;
    let top = hsv(hue, 0.85, if tron { 0.03 } else { 0.22 });
    let low = if tron { hsv(hue, 0.85, 0.5) } else { hsv(hue + rng.between(0.08, 0.2), 0.75, if space { 0.5 } else { 1.0 }) };
    let mut canvas = Canvas { pixels: vec![[0.0; 3]; SIZE * SIZE] };
    for y in 0..SIZE {
        let t = smooth(0.0, horizon, (y as f32 + 0.5) / PX);
        let row = mix(top, low, if tron { t * t } else { t });
        canvas.pixels[y * SIZE..(y + 1) * SIZE].fill(row);
    }
    if space {
        for _ in 0..3 {
            let (cx, cy, radius) = (rng.unit(), rng.unit(), rng.between(0.2, 0.45));
            let color = hsv(hue + rng.between(-0.15, 0.35), 0.8, 1.0);
            canvas.paint_box(cx, cy, radius, radius, |x, y| (color, 0.3 * (1.0 - smooth(0.0, 1.0, (x - cx).hypot(y - cy) / radius))));
        }
    }
    for _ in 0..if space { 160 } else { 70 } {
        let (sy, sx) = (rng.between(0.0, horizon * 0.8), rng.unit());
        let (radius, alpha) = (rng.between(0.6, 1.6) / PX, rng.between(0.4, 1.0));
        canvas.disc(sx, sy, radius, [1.0; 3], alpha);
    }

    // The sun, which may sit partly behind the horizon.
    let (sx, r) = (rng.between(0.25, 0.75), rng.between(0.15, 0.26));
    let sy = horizon.min(0.8) - rng.between(0.08, 0.3);
    if !space {
        sun_disc(&mut canvas, sun_style, (sx, sy, r), &tones);
    }

    // What stands on the horizon.
    match scene {
        Scene::Landscape => {
            let count = 1 + (rng.next() % 4) as usize;
            let sharp = rng.unit() < 0.35;
            for i in 0..count {
                let depth = (count - 1 - i) as f32;
                let amp = if sharp { rng.between(0.05, 0.14) } else { rng.between(0.03, 0.09) };
                let waves = waves(&mut rng, (3.0, 14.0));
                let color = mix(hsv(hue + 0.02 * i as f32, 0.7, 0.45 - 0.12 * depth), hsv(hue, 0.8, (0.25 + 0.2 * i as f32) * 0.3), 0.5);
                hills(&mut canvas, horizon - 0.02 - 0.03 * depth, amp, &waves, sharp, color, None, horizon + 0.01);
            }
        }
        Scene::Skyline => skyline(&mut canvas, &mut rng, horizon, &tones),
        Scene::Tron => neon_towers(&mut canvas, &mut rng, horizon, &tones),
        Scene::Dunes => {
            if rng.unit() < 0.5 {
                pyramid(&mut canvas, &mut rng, horizon + 0.08, &tones);
            }
            let rim = hsv(hue + 0.15, 0.6, 1.0);
            for i in 0..3 {
                let waves = waves(&mut rng, (1.5, 6.0));
                let color = hsv(hue + 0.4 + 0.04 * i as f32, 0.65, 0.5 - 0.15 * i as f32);
                hills(&mut canvas, horizon + 0.07 + 0.11 * i as f32, rng.between(0.06, 0.13), &waves, false, color, Some(rim), 1.0);
            }
        }
        Scene::Space => {}
    }

    // What lies in front of it.
    match scene {
        Scene::Landscape | Scene::Skyline => grid(&mut canvas, horizon, rng.between(0.2, 0.8), rng.between(1.8, 3.5), 0.0, &tones),
        Scene::Tron => {
            let vanish = rng.between(0.3, 0.7);
            grid(&mut canvas, horizon, vanish, rng.between(1.5, 3.0), 0.5, &tones);
            light_trails(&mut canvas, &mut rng, horizon, vanish, hsv(hue + 0.5, 0.9, 1.0));
            haze(&mut canvas, horizon, tones.glow);
        }
        Scene::Dunes | Scene::Space => {}
    }

    // A floating ring and a small moon, or the planet of a space scene.
    let ring_color = hsv(hue + 0.5, 0.15, 1.0);
    if space {
        let (cx, cy, radius) = (rng.between(0.15, 0.85), rng.between(0.3, 0.75), rng.between(0.16, 0.34));
        planet(&mut canvas, &mut rng, (cx, cy, radius), &tones);
        for _ in 0..1 + rng.next() % 2 {
            canvas.disc(rng.between(0.05, 0.95), rng.between(0.05, 0.95), rng.between(0.008, 0.03), hsv(hue + 0.5, 0.2, 1.0), 1.0);
        }
    } else {
        let sky = (horizon * 0.5).max(0.12);
        if scene != Scene::Tron && rng.unit() < 0.7 {
            let rx = rng.between(0.1, 0.18);
            let (cx, cy) = (rng.between(0.2, 0.8), rng.between(0.1, sky));
            let tilt = rng.between(0.18, 0.35);
            canvas.paint_box(cx, cy, rx, rx * tilt, |x, y| {
                let d = ((x - cx) / rx).hypot((y - cy) / (rx * tilt)) * rx * PX;
                (ring_color, coverage(((d - rx * PX).abs()) - 2.2))
            });
        }
        let (mx, my, mr) = (rng.between(0.1, 0.9), rng.between(0.08, sky), rng.between(0.02, 0.05));
        if sun_style != Sun::Twin && rng.unit() < 0.8 {
            canvas.disc(mx, my, mr, hsv(hue + 0.5, 0.2, 1.0), 1.0);
        }
    }

    // Vignette and grain, then 8 bit.
    let mut rgba = Vec::with_capacity(SIZE * SIZE * 4);
    for (i, p) in canvas.pixels.iter().enumerate() {
        let (x, y) = ((i % SIZE) as f32 / PX - 0.5, (i / SIZE) as f32 / PX - 0.5);
        let light = 1.0 - 0.9 * (x * x + y * y);
        let grain = (rng.unit() - 0.5) * 0.04;
        rgba.extend(p.iter().map(|c| ((c * light + grain).clamp(0.0, 1.0) * 255.0 + 0.5) as u8));
        rgba.push(255);
    }
    Thumb { size: [SIZE, SIZE], rgba }
}

/// The sun as `(x, y, radius)`.
fn sun_disc(canvas: &mut Canvas, style: Sun, (sx, sy, r): (f32, f32, f32), tones: &Tones) {
    let fill = |y: f32| mix(tones.sun_top, tones.sun_bottom, smooth(sy - r, sy + r, y));
    match style {
        // Slits that widen towards the bottom.
        Sun::Striped => canvas.paint_box(sx, sy, r, r, |x, y| {
            let t = ((y - (sy - r * 0.1)) / (r * 1.1)).clamp(0.0, 1.0);
            let slit = t > 0.0 && (y / 0.028).fract() < t * 0.7;
            (fill(y), if slit { 0.0 } else { coverage((x - sx).hypot(y - sy) * PX - r * PX) })
        }),
        Sun::Outline => canvas.paint_box(sx, sy, r, r, |x, y| {
            let d = (x - sx).hypot(y - sy) * PX;
            (fill(y), coverage((d - r * PX).abs() - 2.2).max(coverage(d - r * 0.3 * PX)))
        }),
        // A plain sun and a small companion on the side with more room.
        Sun::Twin => {
            canvas.paint_box(sx, sy, r, r, |x, y| (fill(y), coverage((x - sx).hypot(y - sy) * PX - r * PX)));
            let side = if sx < 0.5 { 1.0 } else { -1.0 };
            canvas.disc(sx + side * r * 1.45, sy - r * 0.55, r * 0.4, hsv(tones.hue + 0.45, 0.6, 1.0), 1.0);
        }
        // A dark disc in its own corona.
        Sun::Eclipse => {
            let glow = hsv(tones.hue + 0.1, 0.7, 1.0);
            canvas.paint_box(sx, sy, r * 2.2, r * 2.2, |x, y| (glow, (1.0 - smooth(1.0, 2.2, (x - sx).hypot(y - sy) / r)).powi(2) * 0.85));
            canvas.disc(sx, sy, r * 0.96, hsv(tones.hue, 0.9, 0.04), 1.0);
        }
        Sun::None => {}
    }
}

/// Three sine waves that make the outline of hills.
fn waves(rng: &mut Rng, frequency: (f32, f32)) -> [(f32, f32, f32); 3] {
    std::array::from_fn(|_| (rng.between(frequency.0, frequency.1), rng.between(0.0, 6.3), rng.between(0.2, 0.6)))
}

/// Everything below a wavy line down to `stop`, with a lit edge when `rim` is given. `sharp`
/// swaps the sines for triangle waves, which gives peaks.
#[allow(clippy::too_many_arguments)]
fn hills(canvas: &mut Canvas, base: f32, amp: f32, waves: &[(f32, f32, f32); 3], sharp: bool, color: Rgb, rim: Option<Rgb>, stop: f32) {
    let wave = |v: f32| if sharp { ((v / std::f32::consts::TAU).rem_euclid(1.0) - 0.5).abs() * 4.0 - 1.0 } else { v.sin() };
    let ridge: Vec<f32> = (0..SIZE)
        .map(|x| {
            let ux = (x as f32 + 0.5) / PX;
            base - amp * (0.5 + waves.iter().map(|(f, p, a)| wave(ux * f + p) * a).sum::<f32>() / 2.0)
        })
        .collect();
    for y in 0..SIZE {
        let uy = (y as f32 + 0.5) / PX;
        if uy >= stop {
            break;
        }
        for (x, ridge) in ridge.iter().enumerate() {
            let d = (ridge - uy) * PX;
            canvas.blend(x, y, color, coverage(d));
            if let Some(rim) = rim {
                canvas.blend(x, y, rim, coverage(d.abs() - 1.0) * 0.6);
            }
        }
    }
}

impl Canvas {
    fn rect(&mut self, (x0, y0, x1, y1): (f32, f32, f32, f32), color: Rgb, alpha: f32) {
        let (cx, cy, rx, ry) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0, (x1 - x0) / 2.0, (y1 - y0) / 2.0);
        self.paint_box(cx, cy, rx, ry, |x, y| (color, coverage((((x - cx).abs() - rx).max((y - cy).abs() - ry)) * PX) * alpha));
    }
}

/// Two rows of towers whose bases stand on the horizon; the near row has lit windows and
/// sometimes a mast.
fn skyline(canvas: &mut Canvas, rng: &mut Rng, horizon: f32, tones: &Tones) {
    let window = mix(tones.sun_top, [1.0; 3], 0.4);
    for near in [false, true] {
        let body = if near { hsv(tones.hue + 0.02, 0.6, 0.07) } else { hsv(tones.hue + 0.05, 0.7, 0.28) };
        let mut x = rng.between(-0.04, 0.0);
        while x < 1.0 {
            let width = rng.between(0.035, 0.09) * if near { 1.0 } else { 0.8 };
            let height = if near { rng.between(0.08, 0.34) } else { rng.between(0.05, 0.22) };
            let top = horizon - height;
            canvas.rect((x, top, x + width, horizon + 0.005), body, 1.0);
            if near {
                if rng.unit() < 0.25 {
                    canvas.rect((x + width / 2.0 - 0.002, top - 0.05, x + width / 2.0 + 0.002, top), body, 1.0);
                }
                let (cell_w, cell_h) = (0.014, 0.024);
                for col in 0..((width - 0.016) / cell_w) as usize {
                    for row in 0..((height - 0.02) / cell_h) as usize {
                        if rng.unit() < 0.3 {
                            let (wx, wy) = (x + 0.012 + col as f32 * cell_w, top + 0.014 + row as f32 * cell_h);
                            canvas.rect((wx, wy, wx + 0.007, wy + 0.012), window, 0.9);
                        }
                    }
                }
            }
            x += width;
        }
    }
}

/// A pyramid with a lit and a shaded face; the dunes in front of it hide its base at `base`.
fn pyramid(canvas: &mut Canvas, rng: &mut Rng, base: f32, tones: &Tones) {
    let (cx, height) = (rng.between(0.15, 0.85), rng.between(0.14, 0.28));
    let half = height * rng.between(0.8, 1.1);
    let (lit, shade) = (hsv(tones.hue + 0.45, 0.5, 0.6), hsv(tones.hue + 0.45, 0.7, 0.2));
    canvas.paint_box(cx, base - height / 2.0, half, height / 2.0, |x, y| {
        let reach = ((y - (base - height)) / height).clamp(0.0, 1.0) * half;
        (if x < cx { lit } else { shade }, coverage(((x - cx).abs() - reach) * PX * 0.8) * coverage((y - base) * PX))
    });
}

/// The ground and its grid, which thins towards the horizon. The lines meet at `vanish`.
/// `mirror` is how much of the sky and the horizon's skyline shows in the floor like in glass.
fn grid(canvas: &mut Canvas, horizon: f32, vanish: f32, density: f32, mirror: f32, tones: &Tones) {
    let first = (horizon * PX).ceil() as usize;
    for y in first..SIZE {
        let dy = ((y as f32 + 0.5) / PX - horizon).max(1e-4);
        let fade = smooth(0.0, 0.08, dy);
        let source = (2 * first).checked_sub(y + 1).filter(|_| mirror > 0.0);
        let shine = mirror * (1.0 - smooth(0.0, 0.45, dy));
        for x in 0..SIZE {
            let persp = ((x as f32 + 0.5) / PX - vanish) / dy;
            let lines = ((persp * density + 0.5).fract() - 0.5).abs() * dy * PX / density;
            let rows = (((1.0 / dy) * 0.5).fract() - 0.5).abs() * 2.0 * dy * dy * PX;
            let glow = (1.2 - lines).clamp(0.0, 1.0).max((1.2 - rows).clamp(0.0, 1.0)) * fade;
            let ground = source.map_or(tones.ground, |s| mix(tones.ground, canvas.pixels[s * SIZE + x], shine));
            canvas.pixels[y * SIZE + x] = mix(ground, tones.glow, glow * 0.9);
        }
    }
}

/// Towers with dark faces and glowing edges and floors, in three rows that grow brighter and
/// taller towards the viewer; a few carry a beam of light into the sky.
fn neon_towers(canvas: &mut Canvas, rng: &mut Rng, horizon: f32, tones: &Tones) {
    for (shade, tall) in [(0.35, 0.22), (0.65, 0.32), (1.0, 0.42)] {
        let body = hsv(tones.hue, 0.9, 0.02 + 0.07 * (1.0 - shade));
        let mut x = rng.between(-0.05, 0.0);
        while x < 1.0 {
            let width = rng.between(0.035, 0.1);
            let spire = shade > 0.5 && rng.unit() < 0.15;
            let top = (horizon - rng.between(0.05, tall) * if spire { 1.6 } else { 1.0 }).max(0.05);
            let (cx, cy, rx, ry) = (x + width / 2.0, (top + horizon) / 2.0, width / 2.0, (horizon - top) / 2.0 + 0.0025);
            canvas.rect((x, top, x + width, horizon + 0.005), body, 1.0);
            canvas.paint_box(cx, cy, rx + 0.014, ry + 0.014, |px, py| {
                let edge = ((px - cx).abs() - rx).max((py - cy).abs() - ry).abs();
                (tones.glow, (coverage(edge * PX - 1.2) + 0.45 * (1.0 - smooth(0.0, 0.012, edge))).min(1.0) * shade)
            });
            let mut floor = top + 0.02;
            while floor < horizon - 0.01 {
                if rng.unit() < 0.4 {
                    let inset = rng.between(0.006, 0.02);
                    canvas.rect((x + inset, floor, x + width - inset, floor + 0.0025), tones.glow, 0.55 * shade);
                }
                floor += rng.between(0.018, 0.035);
            }
            if spire {
                canvas.paint_box(cx, top / 2.0, 0.014, top / 2.0, |px, py| {
                    let (across, up) = ((px - cx).abs(), py / top);
                    (tones.glow, (coverage(across * PX - 1.0) * (0.15 + 0.6 * up) + 0.3 * up * (1.0 - smooth(0.0, 0.012, across))).min(1.0))
                });
            }
            x += width;
        }
    }
}

/// Walls of light standing on the floor, as a light cycle leaves them: each runs along a line
/// that meets the grid's vanishing point and is brightest at its head.
fn light_trails(canvas: &mut Canvas, rng: &mut Rng, horizon: f32, vanish: f32, color: Rgb) {
    for _ in 0..2 + rng.next() % 3 {
        let k = if rng.unit() < 0.5 { -1.0 } else { 1.0 } * rng.between(0.4, 2.4);
        let (start, end, height) = (rng.between(0.01, 0.1), rng.between(0.2, 0.5), rng.between(0.12, 0.25));
        for y in (horizon * PX).ceil() as usize..SIZE {
            let dy = (y as f32 + 0.5) / PX - horizon;
            for x in 0..SIZE {
                // Where on the floor the wall under this pixel stands, as its distance below the horizon.
                let floor = ((x as f32 + 0.5) / PX - vanish) / k;
                if floor < start || floor > end || dy > floor || dy < floor * (1.0 - height) {
                    continue;
                }
                let rel = (dy - floor * (1.0 - height)) / (floor * height);
                let edge = (rel.min(1.0 - rel) * floor * height * PX).clamp(0.0, 1.0);
                let head = smooth(0.8, 1.0, (floor - start) / (end - start));
                let rails = ((rel - 0.5).abs() * 2.0).powi(4);
                canvas.blend(x, y, mix(color, [1.0; 3], head * 0.7), edge * (0.5 + 0.5 * rails) * (0.55 + 0.45 * head));
            }
        }
    }
}

/// A bright line on the horizon and the glow above it.
fn haze(canvas: &mut Canvas, horizon: f32, color: Rgb) {
    canvas.paint_box(0.5, horizon, 0.5, 0.05, |_, y| {
        let d = y - horizon;
        let glow = if d < 0.0 { 0.3 * (1.0 - smooth(0.0, 0.05, -d)) } else { 0.0 };
        (color, coverage(d.abs() * PX - 1.0).max(glow))
    });
}

/// A banded planet lit from one side, usually with a tilted ring that passes behind it.
fn planet(canvas: &mut Canvas, rng: &mut Rng, (cx, cy, r): (f32, f32, f32), tones: &Tones) {
    let (c1, c2) = (hsv(tones.hue + 0.15, 0.75, 1.0), hsv(tones.hue + 0.4, 0.8, 0.55));
    let (lx, ly, lz) = (rng.between(-0.8, 0.8), rng.between(-0.7, -0.1), 0.55f32);
    let len = (lx * lx + ly * ly + lz * lz).sqrt();
    let (bands, phase) = (rng.between(5.0, 12.0), rng.between(0.0, 6.3));
    let ring = (rng.unit() < 0.65).then(|| (r * rng.between(1.5, 2.0), rng.between(0.18, 0.32), rng.between(-0.4, 0.4), rng.between(0.62, 0.78)));
    let ring_color = hsv(tones.hue + 0.1, 0.3, 1.0);
    // Colour and alpha of the ring at a pixel, and whether it is the half in front.
    let ring_at = |x: f32, y: f32| {
        let Some((outer, squash, angle, inner)) = ring else { return (ring_color, 0.0, false) };
        let (dx, dy) = (x - cx, y - cy);
        let (u, v) = (dx * angle.cos() + dy * angle.sin(), -dx * angle.sin() + dy * angle.cos());
        let e = (u / outer).hypot(v / (outer * squash));
        let edge = outer * squash * PX;
        let alpha = coverage((e - 1.0) * edge).min(coverage((inner - e) * edge)) * (0.85 + 0.15 * (e * 40.0).sin());
        (mix(ring_color, c1, 0.3 * (e * 17.0).sin().abs()), alpha * 0.85, v > 0.0)
    };
    let reach = ring.map_or(r, |(outer, ..)| outer);
    canvas.paint_box(cx, cy, reach, reach, |x, y| {
        let (color, alpha, _) = ring_at(x, y);
        (color, alpha)
    });
    canvas.paint_box(cx, cy, r, r, |x, y| {
        let (nx, ny) = ((x - cx) / r, (y - cy) / r);
        let d2 = nx * nx + ny * ny;
        let nz = (1.0 - d2).max(0.0).sqrt();
        let lit = (nx * lx + ny * ly + nz * lz) / len;
        let shade = 0.08 + 0.92 * smooth(-0.15, 0.85, lit);
        let t = 0.5 + 0.5 * (ny * bands + 0.35 * (nx * 2.0 + phase).sin()).sin();
        let body = mix(c1, c2, t).map(|c| c * shade);
        (mix(body, tones.glow, (1.0 - nz).powi(3) * 0.4 * shade), coverage((d2.sqrt() - 1.0) * r * PX))
    });
    canvas.paint_box(cx, cy, reach, reach, |x, y| {
        let (color, alpha, front) = ring_at(x, y);
        (color, if front { alpha } else { 0.0 })
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_track_always_gets_the_same_picture() {
        let key = crate::entry_art::key("Pete Oak", "Toro");
        assert_eq!(generate(&key).rgba, generate(&key).rgba);
    }

    #[test]
    fn spelling_details_do_not_change_the_picture_but_tracks_do() {
        let toro = generate(&crate::entry_art::key("Pete Oak", "Toro"));
        assert_eq!(toro.rgba, generate(&crate::entry_art::key("pete  oak", "TORO")).rgba);
        assert_ne!(toro.rgba, generate(&crate::entry_art::key("Pete Oak", "Celsius")).rgba);
    }

    #[test]
    fn the_picture_is_square_opaque_and_not_flat() {
        let t = generate(&crate::entry_art::key("Donnie; Yung Felix", "Troopa (Instrumental)"));
        assert_eq!(t.size, [SIZE, SIZE]);
        assert_eq!(t.rgba.len(), SIZE * SIZE * 4);
        assert!(t.rgba.chunks_exact(4).all(|p| p[3] == 255));
        let luma: Vec<f32> = t.rgba.chunks_exact(4).map(|p| p[0] as f32 + p[1] as f32 + p[2] as f32).collect();
        let mean = luma.iter().sum::<f32>() / luma.len() as f32;
        let variance = luma.iter().map(|l| (l - mean).powi(2)).sum::<f32>() / luma.len() as f32;
        assert!(variance.sqrt() > 40.0, "a landscape has contrast, got {}", variance.sqrt());
        assert!(crate::retro_art::retro_thumb(&t).is_some());
    }

    #[test]
    fn every_scene_and_sun_draws_a_picture() {
        let key = crate::entry_art::key("Nolah", "Lost Island (Original Mix)");
        for scene in [Scene::Landscape, Scene::Skyline, Scene::Tron, Scene::Space, Scene::Dunes] {
            for sun in [Sun::Striped, Sun::Outline, Sun::Twin, Sun::Eclipse, Sun::None] {
                let t = compose(&key, Some(scene), Some(sun));
                assert!(t.rgba.chunks_exact(4).all(|p| p[3] == 255));
                let luma: Vec<f32> = t.rgba.chunks_exact(4).map(|p| p[0] as f32 + p[1] as f32 + p[2] as f32).collect();
                let mean = luma.iter().sum::<f32>() / luma.len() as f32;
                let spread = (luma.iter().map(|l| (l - mean).powi(2)).sum::<f32>() / luma.len() as f32).sqrt();
                assert!(spread > 30.0, "{scene:?} with {sun:?} is flat: {spread}");
            }
        }
    }

    #[test]
    fn tracks_differ_in_kind_and_not_only_in_colour() {
        let kinds: std::collections::HashSet<_> = (0..60).map(|i| {
            let (_, _, scene, sun) = start(&crate::entry_art::key("Artist", &format!("Track {i}")));
            (format!("{scene:?}"), format!("{sun:?}"))
        }).collect();
        let scenes: std::collections::HashSet<_> = kinds.iter().map(|k| &k.0).collect();
        let suns: std::collections::HashSet<_> = kinds.iter().map(|k| &k.1).collect();
        assert_eq!((scenes.len(), suns.len()), (5, 5));
    }

    /// Writes samples to `target/screenshots/placeholder-*.png` to look at: one row per scene
    /// and a sheet of tracks, each in colour and as the dithered cover.
    #[test]
    #[ignore = "writes images; run with --ignored"]
    fn renders_samples() {
        const CELL: u32 = 256;
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/screenshots");
        std::fs::create_dir_all(&dir).unwrap();
        let tracks = [("Pete Oak", "Toro"), ("Donnie; Yung Felix", "Troopa (Instrumental)"), ("Nolah", "Lost Island (Original Mix)"), ("Matan Caspi; Darko De Jan", "Black Street")];
        let scenes = [Scene::Landscape, Scene::Skyline, Scene::Tron, Scene::Tron, Scene::Space, Scene::Dunes];
        let sheet = |name: &str, cols: u32, rows: u32, make: &dyn Fn(u32, u32) -> Thumb| {
            let (mut full, mut retro) = (image::RgbaImage::new(CELL * cols, CELL * rows), image::RgbaImage::new(CELL * cols, CELL * rows));
            for (row, col) in (0..rows).flat_map(|r| (0..cols).map(move |c| (r, c))) {
                let t = make(row, col);
                let r = crate::retro_art::retro_thumb(&t).unwrap();
                let paste = |sheet: &mut image::RgbaImage, t: &Thumb| {
                    let img = image::RgbaImage::from_raw(t.size[0] as u32, t.size[1] as u32, t.rgba.clone()).unwrap();
                    let img = image::imageops::resize(&img, CELL, CELL, image::imageops::FilterType::Triangle);
                    image::imageops::replace(sheet, &img, (col * CELL) as i64, (row * CELL) as i64);
                };
                paste(&mut full, &t);
                paste(&mut retro, &r);
            }
            full.save(dir.join(format!("placeholder-{name}-colour.png"))).unwrap();
            retro.save(dir.join(format!("placeholder-{name}-retro.png"))).unwrap();
        };
        sheet("scenes", 4, 6, &|row, col| compose(&format!("sample {row}/{col}"), Some(scenes[row as usize]), None));
        sheet("tracks", 6, 4, &|row, col| {
            let (artist, title) = tracks[col as usize % 4];
            generate(&crate::entry_art::key(artist, &format!("{title} {}", row * 6 + col)))
        });
    }
}
