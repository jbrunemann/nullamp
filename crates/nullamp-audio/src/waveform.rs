//! A coarse loudness envelope of a whole file, for drawing behind the timeline.
//!
//! Decodes the file once at its own sample rate (no resampling, no channel mapping) and
//! keeps a peak and an RMS value per bucket, plus a colour per bucket from how the sound
//! splits into low, mid and high bands (the red, green and blue of Mixxx's and rekordbox's
//! waveforms). Runs on whatever thread calls it and checks `cancel` between packets, so a
//! track change can abandon a long mix quickly.

use std::path::Path;

use crate::LoadError;
use crate::decode::{Decoding, Produced};

/// Buckets in a full-size waveform: at least one per pixel column of a wide window on a
/// 2x display. Files shorter than this many blocks get fewer.
pub const BUCKETS: usize = 2048;
/// Decode granularity: each block covers this many milliseconds before the blocks are
/// merged into buckets, so the bucket edges are exact whatever the length is.
const BLOCK_MS: u32 = 10;

/// Where the bands part: below `LOW_MID_HZ` is low, above `MID_HIGH_HZ` high.
const LOW_MID_HZ: f64 = 600.0;
const MID_HIGH_HZ: f64 = 4000.0;
/// A band is judged against its own loudness over the file (this share of its buckets are
/// quieter), because bass carries far more energy than hats and would colour everything red.
const BAND_REFERENCE: f32 = 0.95;
/// Above 1 this pulls a bucket's colour towards its dominant band instead of white.
const BAND_CONTRAST: f32 = 1.6;

const MAGIC: &[u8; 4] = b"NWF2";

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum WaveformError {
    #[error(transparent)]
    Load(#[from] LoadError),
    #[error("decoding failed: {0}")]
    Decode(String),
    #[error("no audio in file")]
    Empty,
    #[error("cancelled")]
    Cancelled,
}

/// Loudness over the file's length, `peak` and `rms` of equal length, 0 (silence) to 255 (full scale).
#[derive(Clone, Debug, PartialEq)]
pub struct Waveform {
    pub peak: Vec<u8>,
    pub rms: Vec<u8>,
    /// Red for low sounds, green for mids, blue for highs, one entry per bucket, the brightest
    /// channel at full strength. `None` for a waveform that comes without them.
    pub colors: Option<Vec<[u8; 3]>>,
}

impl Waveform {
    /// Compact form for the disk cache: magic, bucket count (u32 LE), peaks, RMS values, then
    /// optionally the three colour bytes of every bucket.
    pub fn to_bytes(&self) -> Vec<u8> {
        let colors = self.colors.as_ref().filter(|c| c.len() == self.peak.len());
        let mut out = Vec::with_capacity(8 + self.peak.len() * 5);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(self.peak.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.peak);
        out.extend_from_slice(&self.rms);
        if let Some(colors) = colors {
            out.extend(colors.iter().flatten());
        }
        out
    }

    /// `None` for anything that is not exactly what [`to_bytes`](Self::to_bytes) writes,
    /// including an older format, so a stale cache entry is simply recomputed.
    pub fn from_bytes(bytes: &[u8]) -> Option<Waveform> {
        let (head, body) = bytes.split_at_checked(8)?;
        if &head[..4] != MAGIC {
            return None;
        }
        let n = u32::from_le_bytes(head[4..8].try_into().ok()?) as usize;
        if n == 0 || n > BUCKETS || !(body.len() == n * 2 || body.len() == n * 5) {
            return None;
        }
        let colors = (body.len() == n * 5).then(|| body[n * 2..].chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect());
        Some(Waveform { peak: body[..n].to_vec(), rms: body[n..n * 2].to_vec(), colors })
    }
}

/// Decode `path` and summarise it. `cancel` is polled between packets. The band colours cost about
/// half again as much as the rest of the pass, so `with_colors` false leaves them out.
pub fn compute(path: &Path, with_colors: bool, cancel: &dyn Fn() -> bool) -> Result<Waveform, WaveformError> {
    let mut dec = Decoding::open_native(path)?;
    let channels = usize::from(dec.info.channels).max(1);
    let block_frames = (dec.info.sample_rate / (1000 / BLOCK_MS)).max(1) as usize;

    // Per block: peak, and mean of squares (so blocks merge into a bucket's RMS exactly), for the
    // whole signal and for each band of the mono mix.
    let mut blocks: Vec<Block> = Vec::new();
    let (mut peak, mut sum_sq, mut band_sq, mut frames) = (0f32, 0f64, [0f64; 3], 0usize);
    let mut bands = Bands::new(f64::from(dec.info.sample_rate));
    let mut flush = |peak: &mut f32, sum_sq: &mut f64, band_sq: &mut [f64; 3], frames: &mut usize| {
        let mean_sq = *sum_sq / (*frames * channels) as f64;
        blocks.push(Block { peak: *peak, mean_sq: mean_sq as f32, bands: band_sq.map(|s| (s / *frames as f64) as f32) });
        (*peak, *sum_sq, *band_sq, *frames) = (0.0, 0.0, [0.0; 3], 0);
    };

    loop {
        if cancel() {
            return Err(WaveformError::Cancelled);
        }
        match dec.produce().map_err(WaveformError::Decode)? {
            Produced::Eof => break,
            Produced::Data => {
                let new = &dec.ready[dec.ready_pos..];
                for frame in new.chunks_exact(channels) {
                    let mut mono = 0f64;
                    for &s in frame {
                        peak = peak.max(s.abs());
                        sum_sq += f64::from(s) * f64::from(s);
                        mono += f64::from(s);
                    }
                    if with_colors {
                        for (sq, level) in band_sq.iter_mut().zip(bands.split(mono / channels as f64)) {
                            *sq += level * level;
                        }
                    }
                    frames += 1;
                    if frames == block_frames {
                        flush(&mut peak, &mut sum_sq, &mut band_sq, &mut frames);
                    }
                }
                dec.ready_pos = dec.ready.len();
            }
        }
    }
    if frames > 0 {
        flush(&mut peak, &mut sum_sq, &mut band_sq, &mut frames);
    }
    if blocks.is_empty() {
        return Err(WaveformError::Empty);
    }
    Ok(reduce(&blocks, BUCKETS, with_colors))
}

/// What one decode block holds: peak and mean square of the whole signal, and the mean square of each band.
struct Block {
    peak: f32,
    mean_sq: f32,
    bands: [f32; 3],
}

/// A second-order Butterworth filter (RBJ cookbook, transposed direct form II). The state is
/// `f64` because a 200 Hz corner at 44.1 kHz has poles close enough to 1 to lose precision in `f32`.
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    fn new(rate: f64, hz: f64, high: bool) -> Biquad {
        let w = std::f64::consts::TAU * hz.min(rate * 0.45) / rate;
        let (sin, cos) = w.sin_cos();
        let alpha = sin / (2.0 * std::f64::consts::FRAC_1_SQRT_2);
        let a0 = 1.0 + alpha;
        let edge = if high { (1.0 + cos) / 2.0 } else { (1.0 - cos) / 2.0 };
        let middle = if high { -(1.0 + cos) } else { 1.0 - cos };
        Biquad { b: [edge / a0, middle / a0, edge / a0], a: [-2.0 * cos / a0, (1.0 - alpha) / a0], z: [0.0; 2] }
    }

    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// Splits a signal into low, mid and high: the low band is a low-pass, the rest is high-passed
/// once and then divided again, so the mid band is a band-pass and the high band is steeper.
struct Bands {
    low: Biquad,
    rest: Biquad,
    mid: Biquad,
    high: Biquad,
}

impl Bands {
    fn new(rate: f64) -> Bands {
        Bands { low: Biquad::new(rate, LOW_MID_HZ, false), rest: Biquad::new(rate, LOW_MID_HZ, true), mid: Biquad::new(rate, MID_HIGH_HZ, false), high: Biquad::new(rate, MID_HIGH_HZ, true) }
    }

    fn split(&mut self, x: f64) -> [f64; 3] {
        let rest = self.rest.run(x);
        [self.low.run(x), self.mid.run(rest), self.high.run(rest)]
    }
}

/// Merge `blocks` into at most `buckets` equal runs: the loudest peak and the combined RMS of
/// each, and the colour of its bands when `with_colors`.
fn reduce(blocks: &[Block], buckets: usize, with_colors: bool) -> Waveform {
    let n = blocks.len();
    let buckets = n.min(buckets);
    let to_u8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let mut out = Waveform { peak: Vec::with_capacity(buckets), rms: Vec::with_capacity(buckets), colors: None };
    let mut levels = Vec::with_capacity(buckets);
    for b in 0..buckets {
        let lo = b * n / buckets;
        let hi = ((b + 1) * n / buckets).max(lo + 1);
        let run = &blocks[lo..hi];
        let peak = run.iter().map(|b| b.peak).fold(0.0, f32::max);
        let mean_sq = run.iter().map(|b| b.mean_sq).sum::<f32>() / run.len() as f32;
        out.peak.push(to_u8(peak));
        out.rms.push(to_u8(mean_sq.sqrt()));
        levels.push([0, 1, 2].map(|k| (run.iter().map(|b| b.bands[k]).sum::<f32>() / run.len() as f32).sqrt()));
    }
    out.colors = with_colors.then(|| colorize(&levels));
    out
}

/// A colour per bucket from its band levels. Each band is scaled by how loud it gets in this
/// file, so a bucket that is loud for its mids is green even when the bass is louder in absolute
/// terms, and the brightest channel is full, since the bucket's height already carries loudness.
fn colorize(levels: &[[f32; 3]]) -> Vec<[u8; 3]> {
    let gain = [0, 1, 2].map(|k| {
        let mut band: Vec<f32> = levels.iter().map(|l| l[k]).collect();
        band.sort_by(f32::total_cmp);
        let reference = band[((band.len() - 1) as f32 * BAND_REFERENCE) as usize];
        if reference > 1e-6 { 1.0 / reference } else { 0.0 }
    });
    levels
        .iter()
        .map(|l| {
            let scaled = [0, 1, 2].map(|k| (l[k] * gain[k]).powf(BAND_CONTRAST));
            let brightest = scaled.iter().copied().fold(0.0, f32::max);
            if brightest <= 0.0 {
                return [128; 3];
            }
            scaled.map(|s| (s / brightest * 255.0).round() as u8)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures").join(name)
    }

    #[test]
    fn reduce_keeps_loudest_peak_and_combined_rms() {
        let block = |peak, mean_sq| Block { peak, mean_sq, bands: [0.0; 3] };
        let blocks = [block(0.2, 0.01), block(1.0, 0.25), block(0.5, 0.0), block(0.0, 0.0)];
        let w = reduce(&blocks, 2, false);
        assert_eq!(w.peak, [255, 128]);
        // sqrt(mean(0.01, 0.25)) = sqrt(0.13)
        assert_eq!(w.rms, [(0.13f32.sqrt() * 255.0).round() as u8, 0]);
        // Fewer blocks than buckets: one bucket per block.
        assert_eq!(reduce(&blocks[..3], 2048, false).peak.len(), 3);
    }

    #[test]
    fn cache_form_round_trips_and_rejects_damage() {
        let w = Waveform { peak: vec![1, 2, 3], rms: vec![0, 1, 2], colors: Some(vec![[255, 0, 0], [0, 255, 0], [0, 0, 255]]) };
        assert_eq!(Waveform::from_bytes(&w.to_bytes()), Some(w.clone()));
        let plain = Waveform { colors: None, ..w.clone() };
        assert_eq!(Waveform::from_bytes(&plain.to_bytes()), Some(plain));
        let bytes = w.to_bytes();
        assert_eq!(Waveform::from_bytes(&bytes[..bytes.len() - 1]), None);
        assert_eq!(Waveform::from_bytes(b"NWF1\x03\0\0\0abcdef"), None);
        assert_eq!(Waveform::from_bytes(b""), None);
        assert_eq!(Waveform::from_bytes(b"NWF1\0\0\0\0"), None);
    }

    #[test]
    fn tone_has_steady_envelope() {
        let w = compute(&fixture("tone.flac"), true, &|| false).unwrap();
        assert!(!w.peak.is_empty());
        let mid = w.peak.len() / 2;
        // A sine's RMS is its peak over sqrt(2).
        let (p, r) = (f32::from(w.peak[mid]), f32::from(w.rms[mid]));
        assert!(p > 20.0, "tone is audible: {p}");
        assert!((r / p - 0.707).abs() < 0.05, "peak {p}, rms {r}");
    }

    /// Half a second each of a 100 Hz, a 1.5 kHz and a 10 kHz tone: red, then green, then blue.
    #[test]
    fn colours_follow_the_band_that_dominates() {
        let rate = 44100.0;
        let mut blocks = Vec::new();
        let mut bands = Bands::new(rate);
        for hz in [100.0, 1500.0, 10000.0] {
            for b in 0..50 {
                let mut sq = [0f64; 3];
                for i in 0..441 {
                    let t = f64::from(b * 441 + i) / rate;
                    let levels = bands.split(0.5 * (std::f64::consts::TAU * hz * t).sin());
                    for (s, l) in sq.iter_mut().zip(levels) {
                        *s += l * l;
                    }
                }
                blocks.push(Block { peak: 0.5, mean_sq: 0.125, bands: sq.map(|s| (s / 441.0) as f32) });
            }
        }
        let colors = reduce(&blocks, 3, true).colors.unwrap();
        let dominant: Vec<usize> = colors.iter().map(|c| (0..3).max_by_key(|&k| c[k]).unwrap()).collect();
        assert_eq!(dominant, [0, 1, 2], "{colors:?}");
        assert!(colors.iter().all(|c| c.contains(&255)));
    }

    #[test]
    fn decoded_files_come_with_colours() {
        let w = compute(&fixture("tone.flac"), true, &|| false).unwrap();
        assert_eq!(w.colors.as_ref().map(Vec::len), Some(w.peak.len()));
    }

    #[test]
    fn colours_can_be_left_out_without_changing_the_rest() {
        let (with, without) = (compute(&fixture("tone.flac"), true, &|| false).unwrap(), compute(&fixture("tone.flac"), false, &|| false).unwrap());
        assert_eq!(without.colors, None);
        assert_eq!((with.peak, with.rms), (without.peak, without.rms));
    }

    #[test]
    fn mono_and_resample_free_paths_work() {
        // 22.05 kHz mono: decoded at its own rate, not the device's.
        let w = compute(&fixture("tone_mono_22k.wav"), true, &|| false).unwrap();
        assert!(w.peak.iter().any(|&p| p > 20));
    }

    #[test]
    fn cancel_stops_decoding() {
        assert_eq!(compute(&fixture("tone.flac"), true, &|| true), Err(WaveformError::Cancelled));
    }

    #[test]
    fn missing_file_is_a_load_error() {
        assert_eq!(compute(Path::new("/nonexistent/x.mp3"), true, &|| false), Err(WaveformError::Load(LoadError::NotFound)));
    }
}
