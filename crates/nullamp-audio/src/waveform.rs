//! A coarse loudness envelope of a whole file, for drawing behind the timeline.
//!
//! Decodes the file once at its own sample rate (no resampling, no channel mapping) and
//! keeps a peak and an RMS value per bucket. Runs on whatever thread calls it and checks
//! `cancel` between packets, so a track change can abandon a long mix quickly.

use std::path::Path;

use crate::LoadError;
use crate::decode::{Decoding, Produced};

/// Buckets in a full-size waveform: at least one per pixel column of a wide window on a
/// 2x display. Files shorter than this many blocks get fewer.
pub const BUCKETS: usize = 2048;
/// Decode granularity: each block covers this many milliseconds before the blocks are
/// merged into buckets, so the bucket edges are exact whatever the length is.
const BLOCK_MS: u32 = 10;

const MAGIC: &[u8; 4] = b"NWF1";

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
}

impl Waveform {
    /// Compact form for the disk cache: magic, bucket count (u32 LE), peaks, then RMS values.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + self.peak.len() * 2);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(self.peak.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.peak);
        out.extend_from_slice(&self.rms);
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
        if n == 0 || n > BUCKETS || body.len() != n * 2 {
            return None;
        }
        Some(Waveform { peak: body[..n].to_vec(), rms: body[n..].to_vec() })
    }
}

/// Decode `path` and summarise it. `cancel` is polled between packets.
pub fn compute(path: &Path, cancel: &dyn Fn() -> bool) -> Result<Waveform, WaveformError> {
    let mut dec = Decoding::open_native(path)?;
    let channels = usize::from(dec.info.channels).max(1);
    let block_frames = (dec.info.sample_rate / (1000 / BLOCK_MS)).max(1) as usize;

    // Per block: peak, and mean of squares (so blocks merge into a bucket's RMS exactly).
    let mut blocks: Vec<(f32, f32)> = Vec::new();
    let (mut peak, mut sum_sq, mut frames) = (0f32, 0f64, 0usize);
    let mut flush = |peak: &mut f32, sum_sq: &mut f64, frames: &mut usize| {
        let mean_sq = *sum_sq / (*frames * channels) as f64;
        blocks.push((*peak, mean_sq as f32));
        (*peak, *sum_sq, *frames) = (0.0, 0.0, 0);
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
                    for &s in frame {
                        peak = peak.max(s.abs());
                        sum_sq += f64::from(s) * f64::from(s);
                    }
                    frames += 1;
                    if frames == block_frames {
                        flush(&mut peak, &mut sum_sq, &mut frames);
                    }
                }
                dec.ready_pos = dec.ready.len();
            }
        }
    }
    if frames > 0 {
        flush(&mut peak, &mut sum_sq, &mut frames);
    }
    if blocks.is_empty() {
        return Err(WaveformError::Empty);
    }
    Ok(reduce(&blocks, BUCKETS))
}

/// Merge `blocks` into at most `buckets` equal runs: the loudest peak and the combined RMS of each.
fn reduce(blocks: &[(f32, f32)], buckets: usize) -> Waveform {
    let n = blocks.len();
    let buckets = n.min(buckets);
    let to_u8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let mut out = Waveform { peak: Vec::with_capacity(buckets), rms: Vec::with_capacity(buckets) };
    for b in 0..buckets {
        let lo = b * n / buckets;
        let hi = ((b + 1) * n / buckets).max(lo + 1);
        let run = &blocks[lo..hi];
        let peak = run.iter().map(|b| b.0).fold(0.0, f32::max);
        let mean_sq = run.iter().map(|b| b.1).sum::<f32>() / run.len() as f32;
        out.peak.push(to_u8(peak));
        out.rms.push(to_u8(mean_sq.sqrt()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures").join(name)
    }

    #[test]
    fn reduce_keeps_loudest_peak_and_combined_rms() {
        let blocks = [(0.2, 0.01), (1.0, 0.25), (0.5, 0.0), (0.0, 0.0)];
        let w = reduce(&blocks, 2);
        assert_eq!(w.peak, [255, 128]);
        // sqrt(mean(0.01, 0.25)) = sqrt(0.13)
        assert_eq!(w.rms, [(0.13f32.sqrt() * 255.0).round() as u8, 0]);
        // Fewer blocks than buckets: one bucket per block.
        assert_eq!(reduce(&blocks[..3], 2048).peak.len(), 3);
    }

    #[test]
    fn cache_form_round_trips_and_rejects_damage() {
        let w = Waveform { peak: vec![1, 2, 3], rms: vec![0, 1, 2] };
        assert_eq!(Waveform::from_bytes(&w.to_bytes()), Some(w.clone()));
        let bytes = w.to_bytes();
        assert_eq!(Waveform::from_bytes(&bytes[..bytes.len() - 1]), None);
        assert_eq!(Waveform::from_bytes(b"NWF2\x03\0\0\0abcdef"), None);
        assert_eq!(Waveform::from_bytes(b""), None);
        assert_eq!(Waveform::from_bytes(b"NWF1\0\0\0\0"), None);
    }

    #[test]
    fn tone_has_steady_envelope() {
        let w = compute(&fixture("tone.flac"), &|| false).unwrap();
        assert!(!w.peak.is_empty());
        let mid = w.peak.len() / 2;
        // A sine's RMS is its peak over sqrt(2).
        let (p, r) = (f32::from(w.peak[mid]), f32::from(w.rms[mid]));
        assert!(p > 20.0, "tone is audible: {p}");
        assert!((r / p - 0.707).abs() < 0.05, "peak {p}, rms {r}");
    }

    #[test]
    fn mono_and_resample_free_paths_work() {
        // 22.05 kHz mono: decoded at its own rate, not the device's.
        let w = compute(&fixture("tone_mono_22k.wav"), &|| false).unwrap();
        assert!(w.peak.iter().any(|&p| p > 20));
    }

    #[test]
    fn cancel_stops_decoding() {
        assert_eq!(compute(&fixture("tone.flac"), &|| true), Err(WaveformError::Cancelled));
    }

    #[test]
    fn missing_file_is_a_load_error() {
        assert_eq!(compute(Path::new("/nonexistent/x.mp3"), &|| false), Err(WaveformError::Load(LoadError::NotFound)));
    }
}
