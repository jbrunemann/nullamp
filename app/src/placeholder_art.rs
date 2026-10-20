//! The cover of a mix track that no service had a cover for: a retro landscape with a slit
//! sun, ridges, a perspective grid, a floating ring and a moon, drawn from the track's name.
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

/// The cover for the track with this `entry_art::key`.
pub fn generate(key: &str) -> Thumb {
    let mut rng = Rng(crate::store::fnv1a(key.as_bytes()));
    let hue = rng.unit();
    let horizon = rng.between(0.56, 0.7);

    // Sky.
    let top = hsv(hue, 0.85, 0.22);
    let low = hsv(hue + rng.between(0.08, 0.2), 0.75, 1.0);
    let mut canvas = Canvas { pixels: vec![[0.0; 3]; SIZE * SIZE] };
    for y in 0..SIZE {
        let row = mix(top, low, smooth(0.0, horizon, (y as f32 + 0.5) / PX));
        canvas.pixels[y * SIZE..(y + 1) * SIZE].fill(row);
    }
    for _ in 0..70 {
        let (sy, sx) = (rng.between(0.0, horizon * 0.8), rng.unit());
        let (radius, alpha) = (rng.between(0.6, 1.6) / PX, rng.between(0.4, 1.0));
        canvas.disc(sx, sy, radius, [1.0; 3], alpha);
    }

    // The sun, with slits that widen towards its bottom.
    let (sx, r) = (rng.between(0.3, 0.7), rng.between(0.15, 0.26));
    let sy = horizon - rng.between(0.12, 0.24);
    let (sun_top, sun_bottom) = (hsv(hue + 0.12, 0.85, 1.0), hsv(hue + 0.32, 0.9, 1.0));
    canvas.paint_box(sx, sy, r, r, |x, y| {
        let t = ((y - (sy - r * 0.1)) / (r * 1.1)).clamp(0.0, 1.0);
        let slit = t > 0.0 && (y / 0.028).fract() < t * 0.7;
        let alpha = if slit { 0.0 } else { coverage((x - sx).hypot(y - sy) * PX - r * PX) };
        (mix(sun_top, sun_bottom, smooth(sy - r, sy + r, y)), alpha)
    });

    // Three ridges, the far one lightest.
    for i in 0..3 {
        let base = horizon - 0.02 - 0.03 * (2 - i) as f32;
        let amp = rng.between(0.03, 0.09);
        let waves: [(f32, f32, f32); 3] = std::array::from_fn(|_| (rng.between(3.0, 14.0), rng.between(0.0, 6.3), rng.between(0.2, 0.6)));
        let color = mix(hsv(hue + 0.02 * i as f32, 0.7, 0.45 - 0.12 * (2 - i) as f32), hsv(hue, 0.8, (0.25 + 0.2 * i as f32) * 0.3), 0.5);
        for y in 0..SIZE {
            let uy = (y as f32 + 0.5) / PX;
            if uy >= horizon + 0.01 {
                break;
            }
            for x in 0..SIZE {
                let ux = (x as f32 + 0.5) / PX;
                let ridge = base - amp * (0.5 + waves.iter().map(|(f, p, a)| (ux * f + p).sin() * a).sum::<f32>() / 2.0);
                canvas.blend(x, y, color, coverage((ridge - uy) * PX));
            }
        }
    }

    // The ground and its grid, which thins towards the horizon.
    let ground = hsv(hue + 0.5, 0.6, 0.1);
    let glow_color = hsv(hue + 0.5, 0.8, 1.0);
    for y in (horizon * PX).ceil() as usize..SIZE {
        let dy = ((y as f32 + 0.5) / PX - horizon).max(1e-4);
        let fade = smooth(0.0, 0.08, dy);
        for x in 0..SIZE {
            let persp = ((x as f32 + 0.5) / PX - 0.5) / dy;
            let lines = ((persp * 2.5 + 0.5).fract() - 0.5).abs() * dy * PX / 2.5;
            let rows = (((1.0 / dy) * 0.5).fract() - 0.5).abs() * 2.0 * dy * dy * PX;
            let glow = (1.2 - lines).clamp(0.0, 1.0).max((1.2 - rows).clamp(0.0, 1.0)) * fade;
            canvas.pixels[y * SIZE + x] = mix(ground, glow_color, glow * 0.9);
        }
    }

    // A floating ring and a small moon.
    let ring_color = hsv(hue + 0.5, 0.15, 1.0);
    if rng.unit() < 0.8 {
        let rx = rng.between(0.1, 0.18);
        let (cx, cy) = (rng.between(0.2, 0.8), rng.between(0.1, 0.35));
        let tilt = rng.between(0.18, 0.35);
        canvas.paint_box(cx, cy, rx, rx * tilt, |x, y| {
            let d = ((x - cx) / rx).hypot((y - cy) / (rx * tilt)) * rx * PX;
            (ring_color, coverage(((d - rx * PX).abs()) - 2.2))
        });
    }
    let (mx, my, mr) = (rng.between(0.1, 0.9), rng.between(0.08, 0.3), rng.between(0.02, 0.05));
    canvas.disc(mx, my, mr, hsv(hue + 0.5, 0.2, 1.0), 1.0);

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

    /// Writes samples to `target/screenshots/placeholder-*.png` to look at.
    #[test]
    #[ignore = "writes images; run with --ignored"]
    fn renders_samples() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/screenshots");
        std::fs::create_dir_all(&dir).unwrap();
        let tracks = [("Pete Oak", "Toro"), ("Donnie; Yung Felix", "Troopa (Instrumental)"), ("Nolah", "Lost Island (Original Mix)"), ("Matan Caspi; Darko De Jan", "Black Street")];
        let (mut full, mut retro) = (image::RgbaImage::new(SIZE as u32 * 4, SIZE as u32), image::RgbaImage::new(SIZE as u32 * 4, SIZE as u32));
        for (i, (artist, title)) in tracks.iter().enumerate() {
            let t = generate(&crate::entry_art::key(artist, title));
            let r = crate::retro_art::retro_thumb(&t).unwrap();
            let paste = |sheet: &mut image::RgbaImage, t: &Thumb| {
                let img = image::RgbaImage::from_raw(t.size[0] as u32, t.size[1] as u32, t.rgba.clone()).unwrap();
                let img = image::imageops::resize(&img, SIZE as u32, SIZE as u32, image::imageops::FilterType::Nearest);
                image::imageops::replace(sheet, &img, (i * SIZE) as i64, 0);
            };
            paste(&mut full, &t);
            paste(&mut retro, &r);
        }
        full.save(dir.join("placeholder-colour.png")).unwrap();
        retro.save(dir.join("placeholder-retro.png")).unwrap();
    }
}
