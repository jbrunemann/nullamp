//! Musical key detection for dance music.
//!
//! The audio is mixed to mono and decimated to about 11 kHz. In frames of 8192 samples (hopped by
//! half) the spectral peaks are found and refined by parabolic interpolation. Only the strong ones
//! count, those within a tenth of the frame's strongest peak, so drums, noise and the weak
//! overtones stay out, and each frame's peaks are normalised so a loud passage does not outvote a
//! quiet one. The tuning of the track is the circular mean of the peaks' distance from the
//! semitone grid, and the peaks are folded into a 12-bin chroma vector of the whole track.
//!
//! The chroma is correlated with the electronic dance music profiles of Faraldo et al. ("Key
//! Estimation in Electronic Dance Music", ECIR 2016) at all 12 rotations. Dance music is mostly
//! minor and a key shares its notes with its relative, so a major key must beat the best minor one
//! by [`MINOR_BIAS`] to be reported.
//!
//! This replaces the Queen Mary detector of Mixxx ([`crate::key_qm`]): measured against 218 files
//! with keys from tags and from rekordbox it names about 80% of the keys rekordbox names, where
//! the Queen Mary one names 62%. Runs on whatever thread calls it and checks `cancel` between
//! packets.

use std::path::Path;
use std::sync::Arc;

use realfft::{RealFftPlanner, RealToComplex};

use crate::LoadError;
use crate::decode::{Decoding, Produced};

/// Analysis rate the decimation aims for; the actual rate is the file's divided by a whole number.
const TARGET_RATE: f64 = 11_025.0;
const FFT_SIZE: usize = 8192;
const HOP: usize = FFT_SIZE / 2;
/// Peaks outside this range are not notes. The lowest octave is in: a bass note's fundamental
/// is the best evidence of the root, and its third harmonic would otherwise count as the fifth.
const MIN_HZ: f64 = 30.0;
const MAX_HZ: f64 = 4000.0;
/// Only peaks above this say anything about the tuning; below it a semitone is a few bins wide.
const TUNING_MIN_HZ: f64 = 100.0;
/// A peak counts when it is at least this fraction of the strongest in its frame.
const PEAK_FLOOR: f32 = 0.1;
/// MIDI pitches of the lowest and highest notes that count (C1 to B7).
const LOWEST_PITCH: i32 = 24;
const HIGHEST_PITCH: i32 = 107;
/// Correlation a major key must lead the best minor key by.
const MINOR_BIAS: f64 = 0.12;
/// Fewest frames to say anything (about 3 s).
const MIN_FRAMES: usize = 8;
/// Taps of the anti-aliasing filter per unit of the decimation factor.
const TAPS_PER_FACTOR: usize = 8;

/// Key profiles of electronic dance music (Faraldo et al., "edma"), tonic first.
const MAJOR_PROFILE: [f64; 12] = [1.00, 0.29, 0.50, 0.40, 0.60, 0.56, 0.32, 0.80, 0.31, 0.45, 0.42, 0.39];
const MINOR_PROFILE: [f64; 12] = [1.00, 0.31, 0.44, 0.58, 0.33, 0.49, 0.29, 0.78, 0.43, 0.29, 0.53, 0.32];

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum KeyError {
    #[error(transparent)]
    Load(#[from] LoadError),
    #[error("decoding failed: {0}")]
    Decode(String),
    #[error("cancelled")]
    Cancelled,
}

/// The key found for a track.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DetectedKey {
    /// Pitch class of the tonic, C = 0.
    pub tonic: u8,
    pub minor: bool,
    /// How clearly the key won, 0 to 1: the correlation of the track's chroma with the key's
    /// profile. A track that keeps one key scores 0.8 and up.
    pub confidence: f32,
}

/// Find the key of `path`, or of the part of it between `span` (start, end in seconds) when
/// given. `Ok(None)` means nothing could be analysed (the audio is too short or has no notes).
pub fn compute(path: &Path, span: Option<(f64, f64)>, cancel: &dyn Fn() -> bool) -> Result<Option<DetectedKey>, KeyError> {
    let mut dec = Decoding::open_native(path)?;
    let rate = dec.info.sample_rate.max(1);
    let channels = usize::from(dec.info.channels).max(1);
    let mut wanted = u64::MAX;
    if let Some((from, to)) = span {
        if let Err(e) = dec.seek(from) {
            log::debug!("key: cannot seek {}: {e}", path.display());
        }
        wanted = ((to - from).max(0.0) * f64::from(rate)) as u64;
    }
    let mut analysis = Analysis::new(rate);
    let mut frames = 0u64;
    let mut mono = Vec::new();
    while frames < wanted {
        if cancel() {
            return Err(KeyError::Cancelled);
        }
        match dec.produce().map_err(KeyError::Decode)? {
            Produced::Eof => break,
            Produced::Data => {
                mono.clear();
                for frame in dec.ready[dec.ready_pos..].chunks_exact(channels) {
                    if frames >= wanted {
                        break;
                    }
                    mono.push(frame.iter().sum::<f32>() / channels as f32);
                    frames += 1;
                }
                analysis.push(&mono);
                dec.ready_pos = dec.ready.len();
            }
        }
    }
    Ok(analysis.finish())
}

/// Decimates, cuts into frames and collects the strong peaks of each.
struct Analysis {
    down: Downsampler,
    /// Rate after decimation, in Hz.
    rate: f64,
    signal: Vec<f32>,
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    input: Vec<f32>,
    spectrum: Vec<realfft::num_complex::Complex<f32>>,
    magnitude: Vec<f32>,
    frames: usize,
    /// Every counted peak: MIDI pitch (fractional), frame-normalised weight and whether it is
    /// loud enough in pitch to say anything about the tuning.
    peaks: Vec<(f32, f32, bool)>,
}

impl Analysis {
    fn new(sample_rate: u32) -> Self {
        let factor = ((f64::from(sample_rate) / TARGET_RATE).round() as usize).max(1);
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(FFT_SIZE);
        // The symmetric Hann window, which is what the profiles' measurements were made with.
        let window = (0..FFT_SIZE).map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (FFT_SIZE - 1) as f64).cos() as f32).collect();
        Self {
            down: Downsampler::new(factor),
            rate: f64::from(sample_rate) / factor as f64,
            signal: Vec::new(),
            spectrum: fft.make_output_vec(),
            fft,
            window,
            input: vec![0.0; FFT_SIZE],
            magnitude: vec![0.0; FFT_SIZE / 2 + 1],
            frames: 0,
            peaks: Vec::new(),
        }
    }

    fn push(&mut self, mono: &[f32]) {
        self.down.push(mono, &mut self.signal);
        while self.signal.len() >= FFT_SIZE {
            self.frame();
            self.signal.drain(..HOP);
        }
    }

    fn frame(&mut self) {
        for ((x, w), s) in self.input.iter_mut().zip(&self.window).zip(&self.signal) {
            *x = s * w;
        }
        if self.fft.process(&mut self.input, &mut self.spectrum).is_err() {
            return;
        }
        for (m, c) in self.magnitude.iter_mut().zip(&self.spectrum) {
            *m = c.norm();
        }
        self.frames += 1;

        // Local maxima with a parabola through them, as (bin position, amplitude).
        let mut found: Vec<(f64, f32)> = Vec::new();
        for i in 1..self.magnitude.len() - 1 {
            let (a, b, g) = (self.magnitude[i - 1], self.magnitude[i], self.magnitude[i + 1]);
            if b > a && b >= g {
                let den = a - 2.0 * b + g;
                let d = if den != 0.0 { 0.5 * (a - g) / den } else { 0.0 };
                found.push((i as f64 + f64::from(d), b - 0.25 * (a - g) * d));
            }
        }
        let strongest = found.iter().map(|p| p.1).fold(0.0f32, f32::max);
        let bin_hz = self.rate / FFT_SIZE as f64;
        let start = self.peaks.len();
        let mut total = 0.0f32;
        for (bin, amp) in found {
            let hz = bin * bin_hz;
            if !(MIN_HZ..=MAX_HZ).contains(&hz) || amp < PEAK_FLOOR * strongest {
                continue;
            }
            total += amp;
            self.peaks.push(((69.0 + 12.0 * (hz / 440.0).log2()) as f32, amp, hz >= TUNING_MIN_HZ));
        }
        if total > 0.0 {
            self.peaks[start..].iter_mut().for_each(|p| p.1 /= total);
        }
    }

    fn finish(self) -> Option<DetectedKey> {
        if self.frames < MIN_FRAMES {
            return None;
        }
        // Where the track sits against the semitone grid: the angle of the weighted mean of the
        // peaks' phases, which wraps correctly around the half semitone.
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for &(pitch, weight, tunable) in &self.peaks {
            if tunable {
                let phase = 2.0 * std::f64::consts::PI * f64::from(pitch);
                re += f64::from(weight) * phase.cos();
                im += f64::from(weight) * phase.sin();
            }
        }
        let tuning = im.atan2(re) / (2.0 * std::f64::consts::PI);
        let mut chroma = [0.0f64; 12];
        for &(pitch, weight, _) in &self.peaks {
            let note = (f64::from(pitch) - tuning).round() as i32;
            if (LOWEST_PITCH..=HIGHEST_PITCH).contains(&note) {
                chroma[note.rem_euclid(12) as usize] += f64::from(weight);
            }
        }
        classify(&chroma)
    }
}

/// The key whose profile the chroma correlates with best, a major one only when it leads the
/// best minor one by [`MINOR_BIAS`].
fn classify(chroma: &[f64; 12]) -> Option<DetectedKey> {
    let best = |profile: &[f64; 12]| (0..12).map(|tonic| (tonic, correlation(chroma, profile, tonic))).max_by(|a, b| a.1.total_cmp(&b.1)).expect("twelve candidates");
    let (major_tonic, major) = best(&MAJOR_PROFILE);
    let (minor_tonic, minor) = best(&MINOR_PROFILE);
    if major.max(minor) <= 0.0 {
        return None;
    }
    let (tonic, is_minor, score) = if major > minor + MINOR_BIAS { (major_tonic, false, major) } else { (minor_tonic, true, minor) };
    Some(DetectedKey { tonic: tonic as u8, minor: is_minor, confidence: score.clamp(0.0, 1.0) as f32 })
}

/// Pearson correlation of `chroma` with `profile` turned so its tonic is at `tonic`.
fn correlation(chroma: &[f64; 12], profile: &[f64; 12], tonic: usize) -> f64 {
    let mean_c = chroma.iter().sum::<f64>() / 12.0;
    let mean_p = profile.iter().sum::<f64>() / 12.0;
    let (mut num, mut den_c, mut den_p) = (0.0, 0.0, 0.0);
    for (i, &c) in chroma.iter().enumerate() {
        let p = profile[(i + 12 - tonic) % 12] - mean_p;
        let c = c - mean_c;
        num += c * p;
        den_c += c * c;
        den_p += p * p;
    }
    let den = (den_c * den_p).sqrt();
    if den > 0.0 { num / den } else { 0.0 }
}

/// Low-pass filter and keep every `factor`th sample. A windowed sinc with its cutoff at 90% of the
/// new Nyquist, which keeps the hi-hats above it from folding into the notes.
struct Downsampler {
    factor: usize,
    taps: Vec<f32>,
    pending: Vec<f32>,
}

impl Downsampler {
    fn new(factor: usize) -> Self {
        let len = TAPS_PER_FACTOR * factor + 1;
        let cutoff = 0.45 / factor as f64;
        let mid = (len / 2) as f64;
        let taps = (0..len)
            .map(|i| {
                let x = i as f64 - mid;
                let sinc = if x == 0.0 { 2.0 * cutoff } else { (2.0 * std::f64::consts::PI * cutoff * x).sin() / (std::f64::consts::PI * x) };
                let hann = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (len - 1) as f64).cos();
                (sinc * hann) as f32
            })
            .collect();
        Self { factor, taps, pending: Vec::new() }
    }

    fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.factor == 1 {
            out.extend_from_slice(input);
            return;
        }
        self.pending.extend_from_slice(input);
        let mut at = 0;
        while at + self.taps.len() <= self.pending.len() {
            out.push(self.taps.iter().zip(&self.pending[at..]).map(|(t, x)| t * x).sum());
            at += self.factor;
        }
        self.pending.drain(..at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A progression of triads in `tonic` (a major or a minor key), each chord with a few harmonics.
    fn progression(tonic: u8, minor: bool, seconds: f64, rate: f64) -> Vec<f32> {
        let third = if minor { 3 } else { 4 };
        // I, IV, V, I as semitone offsets of their root; triads are root, third, fifth.
        let chords: [(i32, i32); 4] = [(0, third), (5, 4), (7, 4), (0, third)];
        let freq = |semitone: i32| 440.0 * 2f64.powf((f64::from(tonic) + 48.0 + f64::from(semitone) - 69.0) / 12.0);
        let total = (seconds * rate) as usize;
        let chord_len = (total / chords.len()).max(1);
        (0..total)
            .map(|i| {
                let t = i as f64 / rate;
                let (root, interval) = chords[(i / chord_len).min(chords.len() - 1)];
                let mut v = 0.0;
                for (note, weight) in [(root, 1.0), (root + interval, 0.8), (root + 7, 0.9)] {
                    for h in 1..=4 {
                        v += weight / f64::from(h) * (2.0 * std::f64::consts::PI * freq(note) * f64::from(h) * t).sin();
                    }
                }
                (v * 0.1) as f32
            })
            .collect()
    }

    fn detect(samples: &[f32], rate: u32) -> Option<DetectedKey> {
        let mut analysis = Analysis::new(rate);
        samples.chunks(4096).for_each(|c| analysis.push(c));
        analysis.finish()
    }

    #[test]
    fn finds_minor_and_major_keys() {
        for (tonic, minor) in [(9, true), (2, true), (0, true), (6, true), (0, false), (7, false), (3, false)] {
            let key = detect(&progression(tonic, minor, 40.0, 44_100.0), 44_100).expect("a key");
            assert_eq!((key.tonic, key.minor), (tonic, minor), "{tonic} minor={minor}");
        }
    }

    #[test]
    fn works_at_other_sample_rates() {
        for rate in [22_050, 48_000, 96_000] {
            let key = detect(&progression(5, true, 40.0, f64::from(rate)), rate).expect("a key");
            assert_eq!((key.tonic, key.minor), (5, true), "{rate}");
        }
    }

    #[test]
    fn a_detuned_track_has_the_same_key() {
        // A quarter of a semitone flat, which is how a record played slightly slow sounds.
        let flat: Vec<f32> = progression(9, true, 40.0, 44_100.0 * 2f64.powf(0.25 / 12.0));
        let key = detect(&flat, 44_100).expect("a key");
        assert_eq!((key.tonic, key.minor), (9, true));
    }

    #[test]
    fn bass_harmonics_do_not_make_the_fifth_the_key() {
        // A bass line on F with strong overtones (its third harmonic is C) under F minor chords.
        let rate = 44_100.0;
        let mut samples = progression(5, true, 40.0, rate);
        for (i, s) in samples.iter_mut().enumerate() {
            let t = i as f64 / rate;
            let f0 = 43.654;
            *s += (0.15 * (1..=6).map(|h| (2.0 * std::f64::consts::PI * f0 * f64::from(h) * t).sin() / f64::from(h).sqrt()).sum::<f64>()) as f32;
        }
        let key = detect(&samples, 44_100).expect("a key");
        assert_eq!((key.tonic, key.minor), (5, true));
    }

    #[test]
    fn too_little_audio_or_silence_is_not_a_key() {
        assert_eq!(detect(&[], 44_100), None);
        assert_eq!(detect(&vec![0.0; 44_100 * 3], 44_100), None);
        assert_eq!(detect(&vec![0.0; 44_100 * 30], 44_100), None);
    }

    #[test]
    fn a_major_key_needs_to_lead_clearly() {
        // The notes of C major and A minor are the same; with no emphasis on the C the relative minor gets it.
        let mut chroma = [0.0; 12];
        for note in [0, 2, 4, 5, 7, 9, 11] {
            chroma[note] = 1.0;
        }
        let key = classify(&chroma).expect("a key");
        assert!(key.minor, "{key:?}");
    }
}
