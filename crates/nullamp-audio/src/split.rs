//! Where the tracks of a DJ mix start, found from the audio alone (no library, no network).
//!
//! A track repeats itself: its loops, bars and sections come back with the same notes and sounds,
//! while the next track brings other ones. So the mix is cut into blocks of two seconds, each block
//! gets the landmark hashes of its spectral peaks (pairs of peaks, as in Shazam), and two blocks are
//! alike when they share hashes, weighted so that hashes heard all over the mix (a kick drum, a
//! hi-hat) count for little. Blocks are then compared by which other blocks they resemble, which
//! fills in the parts of a track that never repeat each other. Dynamic programming finds the split
//! into N stretches that keeps the most similarity inside them.
//!
//! With the number of tracks known (from a track list) the split is reasonably accurate; without it
//! the number is guessed where one more split stops gaining much, which can be off by several.
//! A 90 minute mix takes about 15 s and a few hundred MB. Runs on whatever thread calls it.

// The dynamic programming reads clearer with indices.
#![allow(clippy::needless_range_loop)]

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::Arc;

use realfft::{RealFftPlanner, RealToComplex};

use crate::LoadError;
use crate::decode::{Decoding, Produced};

const RATE: u32 = 8000;
const FFT_SIZE: usize = 1024;
const HOP: usize = 256;
const FRAME_SECONDS: f64 = HOP as f64 / RATE as f64;
/// Bins kept, about 200 Hz to 3.1 kHz: below is kick drum and bass that every track shares.
const MIN_BIN: usize = 26;
const MAX_BIN: usize = 400;
const BINS: usize = MAX_BIN - MIN_BIN;
const FREQ_RADIUS: usize = 12;
const TIME_RADIUS: usize = 8;
const FLOOR: f32 = -5.0;
const PEAKS_PER_SECOND: usize = 20;
const FAN_OUT: usize = 6;
const MAX_DT: f32 = 63.0;
const MAX_DF: f32 = 62.0;
const FREQ_QUANT: f32 = 2.0;
/// Spectrogram frames searched for peaks at a time; the search needs only their neighbourhood,
/// so the spectrogram of the whole mix is never held.
const CHUNK_FRAMES: usize = 4096;
/// The resolution of the analysis.
pub const BLOCK_SECONDS: f64 = 2.0;
/// Hashes in more than this share of the blocks say nothing about which track plays.
const COMMON_SHARE: f64 = 0.2;
/// Shorter tracks are not looked for, unless the known number of tracks needs them.
const MIN_TRACK_SECONDS: f64 = 60.0;
/// The most tracks a guess considers.
const MAX_GUESS: usize = 30;
/// When guessing, splits that gain less than this share of the largest gain are not considered as the last one.
const GAIN_FLOOR: f64 = 0.2;

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum SplitError {
    #[error(transparent)]
    Load(#[from] LoadError),
    #[error("decoding failed: {0}")]
    Decode(String),
    #[error("cancelled")]
    Cancelled,
}

#[derive(Clone, Copy)]
struct Peak {
    /// Frame, with a fraction.
    t: f32,
    /// Bin, with a fraction.
    f: f32,
    v: f32,
}

/// Where each track of the mix at `path` starts, in seconds, the first at 0. `tracks` is how many
/// there are when known; otherwise the number is guessed. `cancel` is asked between packets and
/// between steps.
pub fn find_starts(path: &Path, tracks: Option<usize>, cancel: &(dyn Fn() -> bool + Sync)) -> Result<Vec<f64>, SplitError> {
    let mut dec = Decoding::open(0, path, RATE, 1)?;
    let mut finder = PeakFinder::new();
    loop {
        if cancel() {
            return Err(SplitError::Cancelled);
        }
        let produced = dec.produce().map_err(SplitError::Decode)?;
        finder.push(&dec.ready[dec.ready_pos..]);
        dec.ready_pos = dec.ready.len();
        if let Produced::Eof = produced {
            break;
        }
    }
    starts(finder, tracks, cancel)
}

fn starts(finder: PeakFinder, tracks: Option<usize>, cancel: &(dyn Fn() -> bool + Sync)) -> Result<Vec<f64>, SplitError> {
    let blocks = {
        let (peaks, samples) = finder.finish();
        block_hashes(&peaks, samples)
    };
    let n = blocks.len();
    if n < 2 {
        return Ok(vec![0.0]);
    }
    let sim = similarity(&blocks);
    drop(blocks);
    if cancel() {
        return Err(SplitError::Cancelled);
    }
    let sim = profiles(&sim, n, cancel).ok_or(SplitError::Cancelled)?;
    let min_len = (MIN_TRACK_SECONDS / BLOCK_SECONDS).round() as usize;
    let bounds = match tracks {
        Some(k) => {
            let k = k.clamp(1, n);
            let mut splits = segment(&sim, n, min_len.min(n / k).max(1), k);
            splits.pop().map(|(_, b)| b).unwrap_or_default()
        }
        None => {
            let mut splits = segment(&sim, n, min_len, MAX_GUESS);
            let k = choose(&splits);
            std::mem::take(&mut splits[k].1)
        }
    };
    Ok(bounds.iter().take(bounds.len().saturating_sub(1)).map(|&b| b as f64 * BLOCK_SECONDS).collect())
}

/// Spectral peaks of mono audio at [`RATE`], fed in pieces. The log spectrum is kept only for the
/// frames still to be searched and their neighbours.
struct PeakFinder {
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    input: Vec<f32>,
    output: Vec<realfft::num_complex::Complex<f32>>,
    /// Samples from `sample_base` on.
    samples: Vec<f32>,
    sample_base: usize,
    total: usize,
    /// Log spectrum rows of the frames from `row_base` to `frames`.
    rows: Vec<f32>,
    row_base: usize,
    frames: usize,
    /// Frames below this have been searched; frame 0 never is, it has no neighbour before it.
    searched: usize,
    found: Vec<Peak>,
}

impl PeakFinder {
    fn new() -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(FFT_SIZE);
        let window = (0..FFT_SIZE).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / FFT_SIZE as f32).cos()).collect();
        let (input, output) = (fft.make_input_vec(), fft.make_output_vec());
        PeakFinder { fft, window, input, output, samples: Vec::new(), sample_base: 0, total: 0, rows: Vec::new(), row_base: 0, frames: 0, searched: 1, found: Vec::new() }
    }

    fn push(&mut self, samples: &[f32]) {
        self.samples.extend_from_slice(samples);
        self.total += samples.len();
        while self.frames * HOP + FFT_SIZE <= self.total {
            let at = self.frames * HOP - self.sample_base;
            for (i, x) in self.input.iter_mut().enumerate() {
                *x = self.samples[at + i] * self.window[i];
            }
            if self.fft.process(&mut self.input, &mut self.output).is_ok() {
                self.rows.extend(self.output[MIN_BIN..MAX_BIN].iter().map(|c| (c.norm() + 1e-9).ln()));
            } else {
                self.rows.extend(std::iter::repeat_n(FLOOR, BINS));
            }
            self.frames += 1;
        }
        let used = (self.frames * HOP).min(self.total);
        self.samples.drain(..used - self.sample_base);
        self.sample_base = used;
        // A frame's peak test looks TIME_RADIUS frames ahead.
        let ready = self.frames.saturating_sub(TIME_RADIUS + 1);
        if ready >= self.searched + CHUNK_FRAMES {
            self.search(ready);
        }
    }

    /// Search the frames from `searched` to `to` and forget the rows no later search needs.
    fn search(&mut self, to: usize) {
        let frames = self.frames - self.row_base;
        // Local maxima: a sliding maximum along frequency, then along time.
        let mut across = vec![0f32; self.rows.len()];
        for frame in 0..frames {
            sliding_max(&self.rows, frame * BINS, 1, BINS, FREQ_RADIUS, &mut across);
        }
        let mut around = vec![0f32; self.rows.len()];
        for bin in 0..BINS {
            sliding_max(&across, bin, BINS, frames, TIME_RADIUS, &mut around);
        }
        let cell = |frame: usize, bin: usize| self.rows[(frame - self.row_base) * BINS + bin];
        for frame in self.searched..to {
            for bin in 1..BINS - 1 {
                let v = cell(frame, bin);
                if v > FLOOR && v >= around[(frame - self.row_base) * BINS + bin] {
                    let t = frame as f32 + parabola(cell(frame - 1, bin), v, cell(frame + 1, bin));
                    let f = (bin + MIN_BIN) as f32 + parabola(cell(frame, bin - 1), v, cell(frame, bin + 1));
                    self.found.push(Peak { t, f, v });
                }
            }
        }
        self.searched = self.searched.max(to);
        let keep = self.searched.saturating_sub(TIME_RADIUS + 1).max(self.row_base);
        self.rows.drain(..(keep - self.row_base) * BINS);
        self.row_base = keep;
    }

    /// The loudest peaks of each second, sorted by time, and how many samples there were.
    fn finish(mut self) -> (Vec<Peak>, usize) {
        let last = self.frames.saturating_sub(1);
        if last > self.searched {
            self.search(last);
        }
        let per_second = (1.0 / FRAME_SECONDS).round() as usize;
        let mut found = std::mem::take(&mut self.found);
        found.sort_by(|a, b| a.t.total_cmp(&b.t));
        let mut kept: Vec<Peak> = Vec::with_capacity(found.len().min(PEAKS_PER_SECOND * (self.frames / per_second + 1)));
        for chunk in found.chunk_by(|a, b| a.t as usize / per_second == b.t as usize / per_second) {
            let mut second = chunk.to_vec();
            second.sort_by(|a, b| b.v.total_cmp(&a.v));
            kept.extend(second.iter().take(PEAKS_PER_SECOND));
        }
        kept.sort_by(|a, b| a.t.total_cmp(&b.t));
        (kept, self.total)
    }
}

/// Writes the maximum of `src[start + k * stride]` over `k` within `radius` of each position.
fn sliding_max(src: &[f32], start: usize, stride: usize, n: usize, radius: usize, dst: &mut [f32]) {
    let at = |k: usize| start + k * stride;
    let mut queue: VecDeque<usize> = VecDeque::new();
    for j in 0..n + radius {
        if j < n {
            while queue.back().is_some_and(|&b| src[at(b)] <= src[at(j)]) {
                queue.pop_back();
            }
            queue.push_back(j);
        }
        if j >= radius {
            let i = j - radius;
            while queue.front().is_some_and(|&f| f + radius < i) {
                queue.pop_front();
            }
            if let Some(&f) = queue.front() {
                dst[at(i)] = src[at(f)];
            }
        }
    }
}

/// Offset of the vertex of the parabola through three neighbours, between -0.5 and 0.5.
fn parabola(before: f32, at: f32, after: f32) -> f32 {
    let curve = before - 2.0 * at + after;
    if curve < 0.0 { (0.5 * (before - after) / curve).clamp(-0.5, 0.5) } else { 0.0 }
}

/// The distinct hashes of the peak pairs anchored in each block, sorted.
fn block_hashes(peaks: &[Peak], samples: usize) -> Vec<Vec<u32>> {
    let n = (samples as f64 / f64::from(RATE) / BLOCK_SECONDS).ceil() as usize;
    let mut blocks = vec![Vec::new(); n];
    for (i, a) in peaks.iter().enumerate() {
        let block = ((f64::from(a.t) * FRAME_SECONDS / BLOCK_SECONDS) as usize).min(n - 1);
        let mut paired = 0;
        for b in &peaks[i + 1..] {
            let dt = (b.t - a.t).round();
            if dt > MAX_DT {
                break;
            }
            let df = b.f - a.f;
            if dt < 1.0 || df.abs() > MAX_DF {
                continue;
            }
            let fq = (a.f / FREQ_QUANT).round() as u32;
            let dfq = ((df / FREQ_QUANT).round() + 32.0) as u32;
            blocks[block].push((fq & 0xff) << 12 | (dfq & 0x3f) << 6 | dt as u32);
            paired += 1;
            if paired == FAN_OUT {
                break;
            }
        }
    }
    for b in &mut blocks {
        b.sort_unstable();
        b.dedup();
    }
    blocks
}

/// Cosine similarity of every two blocks over their shared hashes, each weighted by how rare it is
/// in the mix (inverse document frequency). A block is not compared with itself.
fn similarity(blocks: &[Vec<u32>]) -> Vec<f32> {
    let n = blocks.len();
    let mut where_: HashMap<u32, Vec<u32>> = HashMap::new();
    for (i, b) in blocks.iter().enumerate() {
        for &h in b {
            where_.entry(h).or_default().push(i as u32);
        }
    }
    let common = (COMMON_SHARE * n as f64) as usize;
    let mut sim = vec![0f32; n * n];
    let mut norm = vec![0f64; n];
    for list in where_.values() {
        if list.len() > common {
            continue;
        }
        let w = (n as f64 / list.len() as f64).ln().powi(2);
        for (k, &i) in list.iter().enumerate() {
            norm[i as usize] += w;
            for &j in &list[k + 1..] {
                sim[i as usize * n + j as usize] += w as f32;
            }
        }
    }
    for i in 0..n {
        for j in i + 1..n {
            let s = (f64::from(sim[i * n + j]) / (norm[i] * norm[j]).sqrt().max(1e-12)) as f32;
            sim[i * n + j] = s;
            sim[j * n + i] = s;
        }
    }
    sim
}

/// Cosine similarity of the rows of `sim`: two blocks are alike when they resemble the same other
/// blocks. Parts of a track that never repeat each other still both repeat the rest of it, so a
/// track's square fills in. The costly step (n³), so it runs on several threads. `None` when cancelled.
fn profiles(sim: &[f32], n: usize, cancel: &(dyn Fn() -> bool + Sync)) -> Option<Vec<f32>> {
    let len: Vec<f32> = sim.chunks_exact(n).map(|r| dot(r, r).sqrt().max(1e-12)).collect();
    let threads = std::thread::available_parallelism().map_or(1, |t| t.get()).clamp(1, 8);
    let mut out = vec![0f32; n * n];
    std::thread::scope(|s| {
        // Rows are dealt round-robin, so the shrinking rows of the upper triangle share the work evenly.
        let mut lanes: Vec<Vec<(usize, &mut [f32])>> = (0..threads).map(|_| Vec::new()).collect();
        for (i, row) in out.chunks_exact_mut(n).enumerate() {
            lanes[i % threads].push((i, row));
        }
        let len = &len;
        for lane in lanes {
            s.spawn(move || {
                for (i, row) in lane {
                    if cancel() {
                        return;
                    }
                    let a = &sim[i * n..(i + 1) * n];
                    for j in i + 1..n {
                        row[j] = dot(a, &sim[j * n..(j + 1) * n]) / (len[i] * len[j]);
                    }
                }
            });
        }
    });
    if cancel() {
        return None;
    }
    for i in 0..n {
        for j in i + 1..n {
            out[j * n + i] = out[i * n + j];
        }
    }
    Some(out)
}

/// In eight lanes, so it vectorises.
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut lanes = [0f32; 8];
    for (x, y) in a.chunks_exact(8).zip(b.chunks_exact(8)) {
        for k in 0..8 {
            lanes[k] += x[k] * y[k];
        }
    }
    let tail = a.len() / 8 * 8;
    lanes.iter().sum::<f32>() + a[tail..].iter().zip(&b[tail..]).map(|(x, y)| x * y).sum::<f32>()
}

/// For each number of tracks k up to `most`, the best split into k stretches of at least `min_len`
/// blocks: its score (the sum over stretches of their inner similarity divided by their length)
/// and its bounds, from 0 to `n`. Index 0 is unused; fewer are returned when the mix is too short for `most`.
fn segment(sim: &[f32], n: usize, min_len: usize, most: usize) -> Vec<(f64, Vec<usize>)> {
    // prefix[(i * (n + 1)) + j]: sum of sim over rows below i and columns below j.
    let w = n + 1;
    let mut prefix = vec![0f64; w * w];
    for i in 0..n {
        for j in 0..n {
            prefix[(i + 1) * w + j + 1] = f64::from(sim[i * n + j]) + prefix[i * w + j + 1] + prefix[(i + 1) * w + j] - prefix[i * w + j];
        }
    }
    let score = |a: usize, b: usize| (prefix[b * w + b] - prefix[a * w + b] - prefix[b * w + a] + prefix[a * w + a]) / (b - a) as f64;

    let most = most.min(n / min_len).max(1);
    let mut best = vec![vec![f64::NEG_INFINITY; w]; most + 1];
    let mut from = vec![vec![0usize; w]; most + 1];
    for b in min_len..=n {
        best[1][b] = score(0, b);
    }
    for k in 2..=most {
        for b in k * min_len..=n {
            for a in (k - 1) * min_len..=b - min_len {
                let s = best[k - 1][a] + score(a, b);
                if s > best[k][b] {
                    best[k][b] = s;
                    from[k][b] = a;
                }
            }
        }
    }
    let mut out = vec![(0.0, Vec::new())];
    for k in 1..=most {
        let mut bounds = vec![n];
        let mut b = n;
        for kk in (2..=k).rev() {
            b = from[kk][b];
            bounds.push(b);
        }
        bounds.push(0);
        bounds.reverse();
        out.push((best[k][n], bounds));
    }
    out
}

/// The number of tracks where the gain of one more split drops the most, relative to the gain
/// before. A change between tracks gains a lot and a split inside a track little, so the drop
/// marks where the tracks run out.
fn choose(splits: &[(f64, Vec<usize>)]) -> usize {
    // gains[i]: what going from i + 1 to i + 2 tracks gains.
    let gains: Vec<f64> = splits.windows(2).skip(1).map(|w| w[1].0 - w[0].0).collect();
    let floor = GAIN_FLOOR * gains.iter().copied().fold(0.0, f64::max);
    let mut best = (1, 0.0);
    for (i, pair) in gains.windows(2).enumerate() {
        if pair[0] < floor {
            break;
        }
        let drop = pair[0] / pair[1].max(1e-9);
        if drop > best.1 {
            best = (i + 2, drop);
        }
    }
    best.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A made-up mix: `tracks` stretches of `seconds` each, every one a repeated tune of
    /// quarter-second notes drawn from its own set of pitches, over a quiet noise they all share.
    fn mix(tracks: usize, seconds: f64) -> Vec<f32> {
        let mut seed = 12345u32;
        let mut random = move || {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (seed >> 8) as f32 / (1u32 << 24) as f32
        };
        let rate = RATE as f32;
        let note = (rate * 0.25) as usize;
        let mut out = Vec::new();
        for track in 0..tracks {
            // Between 300 Hz and 3 kHz, the tracks' pitches interleaved.
            let pitches: Vec<f32> = (0..10).map(|p| 300.0 + 2700.0 * (p * tracks + track) as f32 / (10 * tracks) as f32).collect();
            // A two bar phrase that the track repeats, so it is like itself and unlike the others.
            let phrase: Vec<f32> = (0..32).map(|_| pitches[(random() * pitches.len() as f32) as usize % pitches.len()]).collect();
            let notes = (seconds / 0.25) as usize;
            for k in 0..notes {
                let f = phrase[k % phrase.len()];
                for i in 0..note {
                    let t = i as f32 / rate;
                    let fade = (1.0 - i as f32 / note as f32).max(0.0);
                    out.push(0.3 * fade * (std::f32::consts::TAU * f * t).sin() + 0.02 * (random() - 0.5));
                }
            }
        }
        out
    }

    fn analyse(samples: &[f32], tracks: Option<usize>) -> Vec<f64> {
        let mut finder = PeakFinder::new();
        // In odd pieces, as decoded packets come.
        for piece in samples.chunks(1152) {
            finder.push(piece);
        }
        starts(finder, tracks, &|| false).unwrap()
    }

    #[test]
    fn finds_the_changes_of_a_made_up_mix() {
        let samples = mix(6, 80.0);
        for tracks in [Some(6), None] {
            let starts = analyse(&samples, tracks);
            assert_eq!(starts.len(), 6, "{tracks:?}: {starts:?}");
            for (k, s) in starts.iter().enumerate() {
                assert!((s - 80.0 * k as f64).abs() <= 2.0 * BLOCK_SECONDS, "{tracks:?}: {starts:?}");
            }
        }
    }

    #[test]
    fn a_known_count_is_kept_even_below_the_shortest_track_length() {
        // Twelve tracks in four minutes are shorter than a guess would allow.
        let starts = analyse(&mix(12, 20.0), Some(12));
        assert_eq!(starts.len(), 12, "{starts:?}");
        assert!(starts.windows(2).all(|w| w[0] < w[1]), "{starts:?}");
    }

    #[test]
    fn peaks_do_not_depend_on_the_chunking() {
        let samples = mix(2, 70.0);
        let peaks = |piece: usize| {
            let mut finder = PeakFinder::new();
            for p in samples.chunks(piece) {
                finder.push(p);
            }
            finder.finish().0.iter().map(|p| (p.t, p.f)).collect::<Vec<_>>()
        };
        assert_eq!(peaks(1152), peaks(samples.len()));
    }

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures").join(name)
    }

    #[test]
    fn a_short_file_is_one_track_and_cancel_stops() {
        assert_eq!(find_starts(&fixture("tone.flac"), None, &|| false), Ok(vec![0.0]));
        assert_eq!(find_starts(&fixture("tone.flac"), None, &|| true), Err(SplitError::Cancelled));
        assert_eq!(find_starts(Path::new("/nonexistent/x.mp3"), None, &|| false), Err(SplitError::Load(LoadError::NotFound)));
    }
}
