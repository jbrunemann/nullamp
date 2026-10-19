//! The retro rendition of a cover: a 96 pixel, six tone, ordered-dither picture in the LCD's greens.

use image::{GrayImage, imageops::FilterType};

use crate::ui::theme::{LCD_BG, LCD_FG, LCD_MID};
use crate::workers::Thumb;

/// Pixels along the longer side of the dithered picture.
const GRID: u32 = 96;
/// Each dithered pixel is stored as a block this wide, so the texture scales to the deck
/// without uneven pixel sizes at fractional display scales.
const BLOCK: u32 = 3;
const TONES: usize = 6;
/// Share of the darkest and brightest pixels ignored when stretching the contrast, so one
/// stray highlight does not flatten the rest of the cover.
const CUTOFF: f32 = 0.02;
const BAYER4: [[f32; 4]; 4] = [[0.0, 8.0, 2.0, 10.0], [12.0, 4.0, 14.0, 6.0], [3.0, 11.0, 1.0, 9.0], [15.0, 7.0, 13.0, 5.0]];

/// Dark to bright through the display's own colours.
fn palette() -> [[u8; 4]; TONES] {
    let stops = [LCD_BG, LCD_MID, LCD_FG];
    std::array::from_fn(|i| {
        let t = i as f32 / (TONES - 1) as f32 * (stops.len() - 1) as f32;
        let a = (t as usize).min(stops.len() - 2);
        let (from, to, f) = (stops[a], stops[a + 1], t - a as f32);
        let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * f).round() as u8;
        [mix(from.r(), to.r()), mix(from.g(), to.g()), mix(from.b(), to.b()), 255]
    })
}

/// The darkest and brightest luma that survive the cutoff.
fn contrast_range(img: &GrayImage) -> (f32, f32) {
    let mut hist = [0u32; 256];
    for p in img.pixels() {
        hist[p.0[0] as usize] += 1;
    }
    let skip = (img.width() * img.height()) as f32 * CUTOFF;
    let edge = |order: &mut dyn Iterator<Item = usize>| {
        let mut seen = 0.0;
        for v in order {
            seen += hist[v] as f32;
            if seen > skip {
                return Some(v);
            }
        }
        None
    };
    match (edge(&mut (0..256)), edge(&mut (0..256).rev())) {
        (Some(lo), Some(hi)) if hi > lo => (lo as f32, hi as f32),
        _ => (0.0, 255.0),
    }
}

/// `None` for an empty image.
pub fn retro_thumb(src: &Thumb) -> Option<Thumb> {
    let [w, h] = src.size;
    if w == 0 || h == 0 || src.rgba.len() != w * h * 4 {
        return None;
    }
    let luma: Vec<u8> = src.rgba.chunks_exact(4).map(|p| (0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32).round() as u8).collect();
    let gray = GrayImage::from_raw(w as u32, h as u32, luma)?;
    let (gw, gh) = if w >= h { (GRID, (GRID as usize * h / w).max(1) as u32) } else { ((GRID as usize * w / h).max(1) as u32, GRID) };
    let small = image::imageops::resize(&gray, gw, gh, FilterType::Triangle);
    let (lo, hi) = contrast_range(&small);
    let palette = palette();

    let (out_w, out_h) = ((gw * BLOCK) as usize, (gh * BLOCK) as usize);
    let mut rgba = vec![0u8; out_w * out_h * 4];
    for (x, y, p) in small.enumerate_pixels() {
        let v = ((p.0[0] as f32 - lo) / (hi - lo)).clamp(0.0, 1.0);
        let threshold = BAYER4[(y % 4) as usize][(x % 4) as usize] / 16.0 - 0.5;
        let tone = (v * (TONES - 1) as f32 + threshold * 0.9).round().clamp(0.0, (TONES - 1) as f32) as usize;
        for by in 0..BLOCK {
            let row = (y * BLOCK + by) as usize * out_w;
            for bx in 0..BLOCK {
                let at = (row + (x * BLOCK + bx) as usize) * 4;
                rgba[at..at + 4].copy_from_slice(&palette[tone]);
            }
        }
    }
    Some(Thumb { size: [out_w, out_h], rgba })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(w: usize, h: usize) -> Thumb {
        let mut rgba = Vec::with_capacity(w * h * 4);
        for _ in 0..h {
            for x in 0..w {
                let v = (x * 255 / (w - 1)) as u8;
                rgba.extend([v, v / 2, 255 - v, 255]);
            }
        }
        Thumb { size: [w, h], rgba }
    }

    #[test]
    fn fits_the_grid_and_keeps_the_aspect_ratio() {
        let wide = retro_thumb(&gradient(400, 200)).unwrap();
        assert_eq!(wide.size, [(GRID * BLOCK) as usize, (GRID / 2 * BLOCK) as usize]);
        let tall = retro_thumb(&gradient(100, 400)).unwrap();
        assert_eq!(tall.size, [(GRID / 4 * BLOCK) as usize, (GRID * BLOCK) as usize]);
    }

    #[test]
    fn uses_only_palette_colours_and_spans_dark_to_bright() {
        let out = retro_thumb(&gradient(256, 256)).unwrap();
        let palette = palette();
        let mut seen = [false; TONES];
        for p in out.rgba.chunks_exact(4) {
            let tone = palette.iter().position(|c| c == p).expect("every pixel is a palette colour");
            seen[tone] = true;
        }
        assert!(seen.iter().all(|s| *s), "a full gradient uses all {TONES} tones");
    }

    #[test]
    fn flat_and_degenerate_images_do_not_panic() {
        let flat = Thumb { size: [8, 8], rgba: [90, 90, 90, 255].repeat(64) };
        assert!(retro_thumb(&flat).is_some());
        assert!(retro_thumb(&Thumb { size: [0, 0], rgba: Vec::new() }).is_none());
        assert!(retro_thumb(&Thumb { size: [4, 4], rgba: vec![0; 3] }).is_none());
    }
}
