//! Musical key detection the way Mixxx does it, kept as the reference for [`crate::key`].
//!
//! `confidence` is the share of the analysed audio in which the winning key was the reading.
//!
//! A port of the Queen Mary key detector (QM-DSP `GetKeyMode`, `Chromagram`, `ConstantQ` and
//! `Decimator`, Centre for Digital Music, Queen Mary, University of London, by Christian Landone,
//! Katy Noland and Daniel Schürmann) and of the way Mixxx drives it (`AnalyzerQueenMaryKey`,
//! `DownmixAndOverlapHelper`, `KeyUtils::calculateGlobalKey`), from the Mixxx sources
//! (https://github.com/mixxxdj/mixxx, GPL-2.0-or-later).
//!
//! The audio is mixed to mono and decimated by 8 with an 8th-order IIR filter. Every block of
//! decimated samples goes through a constant-Q transform with 36 bins per octave over four
//! octaves from C3 (a third of a semitone below, so that three bins make a note), and the octaves
//! are folded into one 36-bin chroma vector. The chroma is averaged over about 10 s, correlated
//! with a major and a minor profile at all 36 shifts, and the best of the 24 keys is median
//! filtered over another 10 s. The key of the track is the one that held the longest.

use std::f64::consts::PI;
use std::path::Path;
use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

use crate::decode::{Decoding, Produced};
use crate::key::{DetectedKey, KeyError};

const BINS_PER_OCTAVE: usize = 36;
const DECIMATION: usize = 8;
/// Kernel cells whose magnitude is at most this are dropped, which keeps the kernel sparse.
const CQ_THRESHOLD: f64 = 0.0054;
const TUNING_HZ: f32 = 440.0;
/// Seconds the chroma is averaged over, and the median filter of the keys is long.
const AVERAGE_SECONDS: f64 = 10.0;

/// Chroma profiles of a major and a minor key, three bins per note, the tonic's centre at bin 1.
const MAJOR_PROFILE: [f64; BINS_PER_OCTAVE] = [
    0.0384, 0.0629, 0.0258, 0.0121, 0.0146, 0.0106, 0.0364, 0.0610, 0.0267, 0.0126, 0.0121, 0.0086, 0.0364, 0.0623, 0.0279, 0.0275, 0.0414, 0.0186, 0.0173, 0.0248, 0.0145, 0.0364, 0.0631,
    0.0262, 0.0129, 0.0150, 0.0098, 0.0312, 0.0521, 0.0235, 0.0129, 0.0142, 0.0095, 0.0289, 0.0478, 0.0239,
];
const MINOR_PROFILE: [f64; BINS_PER_OCTAVE] = [
    0.0375, 0.0682, 0.0299, 0.0119, 0.0138, 0.0093, 0.0296, 0.0543, 0.0257, 0.0292, 0.0519, 0.0246, 0.0159, 0.0234, 0.0135, 0.0291, 0.0544, 0.0248, 0.0137, 0.0176, 0.0104, 0.0352, 0.0670,
    0.0302, 0.0222, 0.0349, 0.0164, 0.0174, 0.0297, 0.0166, 0.0222, 0.0401, 0.0202, 0.0175, 0.0270, 0.0146,
];

/// Find the key of `path`, or of the part of it between `span` (start, end in seconds) when
/// given. `Ok(None)` means nothing could be analysed (the audio is too short).
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
    let mut analysis = Analysis::new(f64::from(rate));
    while analysis.frames < wanted {
        if cancel() {
            return Err(KeyError::Cancelled);
        }
        match dec.produce().map_err(KeyError::Decode)? {
            Produced::Eof => break,
            Produced::Data => {
                for frame in dec.ready[dec.ready_pos..].chunks_exact(channels) {
                    if analysis.frames >= wanted {
                        break;
                    }
                    analysis.push(frame.iter().sum::<f32>() / channels as f32);
                }
                dec.ready_pos = dec.ready.len();
            }
        }
    }
    Ok(analysis.finish())
}

/// Mixxx's `DownmixAndOverlapHelper` around the detector: windows of `block` frames that follow
/// each other without overlap, the first one centred on the start (half of it silence), and the
/// end padded with silence so the last samples count. Keeps the frames at which the key changed.
struct Analysis {
    detector: Detector,
    buffer: Vec<f64>,
    write: usize,
    frames: u64,
    previous: u8,
    changes: Vec<(u8, u64)>,
}

impl Analysis {
    fn new(sample_rate: f64) -> Self {
        let detector = Detector::new(sample_rate);
        let block = detector.block;
        Self { detector, buffer: vec![0.0; block], write: block / 2, frames: 0, previous: 0, changes: Vec::new() }
    }

    fn push(&mut self, sample: f32) {
        self.frames += 1;
        self.put(f64::from(sample));
    }

    fn put(&mut self, sample: f64) {
        self.buffer[self.write] = sample;
        self.write += 1;
        if self.write == self.buffer.len() {
            let key = self.detector.process(&self.buffer);
            if key != self.previous {
                self.changes.push((key, self.frames));
                self.previous = key;
            }
            self.write = 0;
        }
    }

    fn finish(mut self) -> Option<DetectedKey> {
        let block = self.buffer.len();
        for _ in 0..(block - self.write).max(block / 2 - 1) {
            self.put(0.0);
        }
        global_key(&self.changes, self.frames)
    }
}

/// `KeyUtils::calculateGlobalKey`: the key that held the longest, the lower one on a tie.
fn global_key(changes: &[(u8, u64)], total: u64) -> Option<DetectedKey> {
    let mut held = [0u64; 25];
    for (i, &(key, start)) in changes.iter().enumerate() {
        let end = changes.get(i + 1).map_or(total, |next| next.1);
        held[usize::from(key)] += end.saturating_sub(start);
    }
    let (mut best, mut longest) = (0usize, 0u64);
    for (key, &frames) in held.iter().enumerate().skip(1) {
        if frames > longest {
            (best, longest) = (key, frames);
        }
    }
    (best != 0).then(|| DetectedKey { tonic: ((best - 1) % 12) as u8, minor: best > 12, confidence: longest as f32 / total.max(1) as f32 })
}

/// `GetKeyMode`: a key from 1 (C major) to 24 (B minor) for each block of audio.
struct Detector {
    /// Frames of audio per call, [`Chroma::fft_len`] decimated by [`DECIMATION`].
    block: usize,
    decimator: Decimator,
    chroma: Chroma,
    decimated: Vec<f64>,
    /// The last chroma vectors, a ring of `chroma_len` of them, `filled` valid.
    history: Vec<f64>,
    chroma_len: usize,
    filled: usize,
    next: usize,
    major: [f64; BINS_PER_OCTAVE],
    minor: [f64; BINS_PER_OCTAVE],
    /// The last keys, oldest first, `keys_filled` valid.
    keys: Vec<u8>,
    keys_filled: usize,
}

impl Detector {
    fn new(sample_rate: f64) -> Self {
        let chroma = Chroma::new((sample_rate / DECIMATION as f64).max(1.0));
        let frame = chroma.fft_len;
        let window = (AVERAGE_SECONDS * (sample_rate / DECIMATION as f64).max(1.0) / frame as f64).ceil() as usize;
        let centred = |profile: &[f64; BINS_PER_OCTAVE]| {
            let mean = profile.iter().sum::<f64>() / BINS_PER_OCTAVE as f64;
            profile.map(|v| v - mean)
        };
        Self {
            block: frame * DECIMATION,
            decimator: Decimator::new(),
            decimated: vec![0.0; frame],
            history: vec![0.0; BINS_PER_OCTAVE * window],
            chroma_len: window,
            filled: 0,
            next: 0,
            major: centred(&MAJOR_PROFILE),
            minor: centred(&MINOR_PROFILE),
            keys: vec![0; window],
            keys_filled: 0,
            chroma,
        }
    }

    fn process(&mut self, pcm: &[f64]) -> u8 {
        self.decimator.process(pcm, &mut self.decimated);
        let chroma = self.chroma.process(&self.decimated);
        self.history[self.next * BINS_PER_OCTAVE..][..BINS_PER_OCTAVE].copy_from_slice(&chroma);
        self.next = if self.next >= self.chroma_len - 1 { 0 } else { self.next + 1 };
        self.filled = (self.filled + 1).min(self.chroma_len);

        let mut mean = [0.0; BINS_PER_OCTAVE];
        for (k, m) in mean.iter_mut().enumerate() {
            *m = (0..self.filled).map(|j| self.history[k + j * BINS_PER_OCTAVE]).sum::<f64>() / self.filled as f64;
        }
        let average = mean.iter().sum::<f64>() / BINS_PER_OCTAVE as f64;
        mean.iter_mut().for_each(|m| *m -= average);

        // Chroma and profiles have the centre of C at bin 1, and the correlation for C is wanted there too.
        let (mut best_major, mut major_bin) = (f64::NEG_INFINITY, 0);
        let (mut best_minor, mut minor_bin) = (f64::NEG_INFINITY, 0);
        for k in 0..BINS_PER_OCTAVE {
            let shift = k as i32 - 1;
            let major = correlation(&mean, &self.major, shift);
            let minor = correlation(&mean, &self.minor, shift);
            if major > best_major {
                (best_major, major_bin) = (major, k);
            }
            if minor > best_minor {
                (best_minor, minor_bin) = (minor, k);
            }
        }
        let bin = if best_major > best_minor { major_bin } else { minor_bin + BINS_PER_OCTAVE };
        let key = (bin / 3 + 1) as u8;

        // The median of the last keys, all of them while fewer have come.
        self.keys.rotate_left(1);
        *self.keys.last_mut().expect("the window is never empty") = key;
        self.keys_filled = (self.keys_filled + 1).min(self.keys.len());
        let mut sorted: Vec<u8> = self.keys[self.keys.len() - self.keys_filled..].to_vec();
        sorted.sort_unstable();
        sorted[(self.keys_filled.div_ceil(2)).max(1) - 1]
    }
}

/// `GetKeyMode::krumCorr`: the Pearson correlation of zero-mean `data` with `profile` shifted by `shift` bins.
fn correlation(data: &[f64], profile: &[f64; BINS_PER_OCTAVE], shift: i32) -> f64 {
    let (mut num, mut sum_data, mut sum_profile) = (0.0, 0.0, 0.0);
    for (i, &d) in data.iter().enumerate() {
        let p = profile[(i as i32 - shift + BINS_PER_OCTAVE as i32) as usize % BINS_PER_OCTAVE];
        num += d * p;
        sum_data += d * d;
        sum_profile += p * p;
    }
    let den = (sum_data * sum_profile).sqrt();
    if den > 0.0 { num / den } else { 0.0 }
}

/// `Chromagram` over a `ConstantQ`: one 36-bin chroma vector per frame of audio.
struct Chroma {
    /// FFT length, which is also the frame the constant-Q transform needs.
    fft_len: usize,
    fft: Arc<dyn Fft<f64>>,
    window: Vec<f64>,
    /// Octaves of constant-Q bins that are folded into the chroma.
    octaves: usize,
    kernel: Vec<Cell>,
    spectrum: Vec<Complex<f64>>,
    cq: Vec<Complex<f64>>,
}

/// A non-zero cell of the sparse kernel: constant-Q bin `row` from FFT bin `col`.
struct Cell {
    col: u32,
    row: u16,
    value: Complex<f64>,
}

fn pitch_frequency(midi: i32, cents: f32, concert_a: f32) -> f32 {
    let p = midi as f32 + cents / 100.0;
    concert_a * 2f32.powf((p - 69.0) / 12.0)
}

fn hamming(n: usize) -> Vec<f64> {
    (0..n).map(|i| if n > 1 { 0.54 - 0.46 * (2.0 * PI * i as f64 / n as f64).cos() } else { 1.0 }).collect()
}

impl Chroma {
    fn new(rate: f64) -> Self {
        // C3 as the base, a third of a semitone lower, so the three bins of a note are centred on it.
        let cents = -12.0f32 / BINS_PER_OCTAVE as f32 * 100.0;
        let min = f64::from(pitch_frequency(48, cents, TUNING_HZ));
        let max = f64::from(pitch_frequency(96, cents, TUNING_HZ));
        // Extended to a whole number of octaves.
        let octaves = (max / min).ln() / 2f64.ln();
        let max = min * 2f64.powf(octaves.ceil());

        let bpo = BINS_PER_OCTAVE as f64;
        let q = 1.0 / (2f64.powf(1.0 / bpo) - 1.0);
        let bins = (bpo * (max / min).ln() / 2f64.ln()).ceil() as usize;
        let fft_len = ((q * rate / min).ceil() as usize).next_power_of_two();

        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(fft_len);
        let kernel = sparse_kernel(&*fft, fft_len, bins, rate, min, q);
        Self {
            fft_len,
            fft,
            window: hamming(fft_len),
            octaves: bins / BINS_PER_OCTAVE,
            kernel,
            spectrum: vec![Complex::new(0.0, 0.0); fft_len],
            cq: vec![Complex::new(0.0, 0.0); bins],
        }
    }

    fn process(&mut self, data: &[f64]) -> [f64; BINS_PER_OCTAVE] {
        let n = self.fft_len;
        // The kernel expects input that has been fftshifted, so swap the halves.
        for (i, (d, w)) in data.iter().zip(&self.window).enumerate() {
            self.spectrum[(i + n / 2) % n] = Complex::new(d * w, 0.0);
        }
        self.fft.process(&mut self.spectrum);

        self.cq.fill(Complex::new(0.0, 0.0));
        for cell in &self.kernel {
            if cell.col == 0 {
                continue;
            }
            self.cq[usize::from(cell.row)] += cell.value * self.spectrum[n - cell.col as usize];
        }

        let mut chroma = [0.0; BINS_PER_OCTAVE];
        for octave in 0..self.octaves {
            for (i, c) in chroma.iter_mut().enumerate() {
                *c += self.cq[octave * BINS_PER_OCTAVE + i].norm();
            }
        }
        let max = chroma.iter().fold(0.0f64, |m, c| m.max(c.abs()));
        if max != 0.0 {
            chroma.iter_mut().for_each(|c| *c /= max);
        }
        chroma
    }
}

/// `ConstantQ::sparsekernel`: for each constant-Q bin a Hamming-windowed complex sinusoid of
/// the right length, transformed to the frequency domain and thresholded.
fn sparse_kernel(fft: &dyn Fft<f64>, n: usize, bins: usize, rate: f64, min: f64, q: f64) -> Vec<Cell> {
    let threshold = CQ_THRESHOLD * CQ_THRESHOLD;
    let mut cells = Vec::new();
    let mut buf = vec![Complex::new(0.0, 0.0); n];
    for j in (0..bins).rev() {
        buf.fill(Complex::new(0.0, 0.0));
        let samples_per_cycle = rate / (min * 2f64.powf(j as f64 / BINS_PER_OCTAVE as f64));
        let length = (q * samples_per_cycle).ceil() as usize;
        let origin = n / 2 - length / 2;
        let window = hamming(length);
        for (i, w) in window.iter().enumerate() {
            let angle = 2.0 * PI * i as f64 / samples_per_cycle;
            // Shaped by the window, scaled by the length, and fftshifted like the input.
            buf[(origin + i + n / 2) % n] = Complex::new(angle.cos(), angle.sin()) * (w / length as f64);
        }
        fft.process(&mut buf);
        for (i, v) in buf.iter().enumerate() {
            if v.norm_sqr() <= threshold {
                continue;
            }
            // The conjugate, normalised.
            cells.push(Cell { col: i as u32, row: j as u16, value: Complex::new(v.re / n as f64, -v.im / n as f64) });
        }
    }
    cells
}

/// `Decimator` for a factor of 8: an 8th-order IIR low-pass in transposed direct form II, then
/// every 8th sample. The filter keeps its state from block to block.
struct Decimator {
    state: [f64; 7],
}

const DECIMATOR_B: [f64; 8] = [0.060111378492136, -0.257323420830598, 0.420583503165928, -0.222750785197418, -0.222750785197418, 0.420583503165928, -0.257323420830598, 0.060111378492136];
const DECIMATOR_A: [f64; 8] = [1.0, -5.667654878577432, 14.062452278088417, -19.737303840697738, 16.88969887460864, -8.796600612325928, 2.577553446979888, -0.326903916815751];

impl Decimator {
    fn new() -> Self {
        Self { state: [0.0; 7] }
    }

    /// Filters all of `input` and writes every [`DECIMATION`]th output into `output`.
    fn process(&mut self, input: &[f64], output: &mut [f64]) {
        let o = &mut self.state;
        for (i, &x) in input.iter().enumerate() {
            let y = x * DECIMATOR_B[0] + o[0];
            for k in 0..6 {
                o[k] = x * DECIMATOR_B[k + 1] - y * DECIMATOR_A[k + 1] + o[k + 1];
            }
            o[6] = x * DECIMATOR_B[7] - y * DECIMATOR_A[7];
            if i % DECIMATION == 0 {
                output[i / DECIMATION] = y;
            }
        }
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
        let a = 440.0;
        let freq = |semitone: i32| a * 2f64.powf((f64::from(tonic) + 48.0 + f64::from(semitone) - 69.0) / 12.0);
        let total = (seconds * rate) as usize;
        let chord_len = (total / chords.len()).max(1);
        (0..total)
            .map(|i| {
                let t = i as f64 / rate;
                let (root, interval) = chords[(i / chord_len).min(chords.len() - 1)];
                let mut v = 0.0;
                for (note, weight) in [(root, 1.0), (root + interval, 0.8), (root + 7, 0.9)] {
                    for h in 1..=4 {
                        v += weight / f64::from(h) * (2.0 * PI * freq(note) * f64::from(h) * t).sin();
                    }
                }
                (v * 0.1) as f32
            })
            .collect()
    }

    fn detect(samples: &[f32], rate: f64) -> Option<DetectedKey> {
        let mut analysis = Analysis::new(rate);
        samples.iter().for_each(|&s| analysis.push(s));
        analysis.finish()
    }

    #[test]
    fn the_constant_q_kernel_covers_four_octaves() {
        let chroma = Chroma::new(44_100.0 / 8.0);
        assert_eq!(chroma.fft_len, 4096);
        assert_eq!(chroma.octaves, 4);
        assert_eq!(chroma.cq.len(), 144);
    }

    #[test]
    fn finds_major_and_minor_keys() {
        for (tonic, minor) in [(0, false), (9, true), (7, false), (2, true), (3, false), (6, true)] {
            let key = detect(&progression(tonic, minor, 40.0, 44_100.0), 44_100.0).expect("a key");
            assert_eq!((key.tonic, key.minor), (tonic, minor), "{tonic} minor={minor}");
        }
    }

    #[test]
    fn works_at_other_sample_rates() {
        for rate in [22_050.0, 48_000.0, 96_000.0] {
            let key = detect(&progression(5, false, 40.0, rate), rate).expect("a key");
            assert_eq!((key.tonic, key.minor), (5, false), "{rate}");
        }
    }

    #[test]
    fn nothing_to_analyse_is_not_a_key() {
        assert_eq!(detect(&[], 44_100.0), None);
        // Silence has no chroma to correlate; it must come out without a panic, whatever it names.
        detect(&vec![0.0; 44_100 * 30], 44_100.0);
    }

    #[test]
    fn the_longest_key_wins_and_a_tie_goes_to_the_lower_one() {
        let changes = [(5u8, 0u64), (3, 100), (5, 150)];
        let key = global_key(&changes, 200).expect("a key");
        assert_eq!((key.tonic, key.minor), (4, false));
        let changes = [(14u8, 0u64), (2, 100)];
        let key = global_key(&changes, 200).expect("a key");
        assert_eq!((key.tonic, key.minor), (1, false));
    }
}
