//! The colours of a waveform: how loud the low, mid and high sounds of each stretch of a file are.
//!
//! Follows Mixxx's waveform analysis (`AnalyzerWaveform`, GPL-2.0-or-later; the filters are in
//! [`crate::fidlib`]). The signal goes through three filters side by side (low below 600 Hz, mid
//! from 600 Hz to 4 kHz, high above 4 kHz, each a 4th-order Bessel), and every stretch of 1/441 s
//! (a stride) keeps the largest absolute value of each band, and of the whole signal, over both
//! channels. The values of a bucket of the overview are the mean of its strides' maxima, never
//! rescaled by anything about the file, so a bass-heavy track stays red. A bucket's colour is its
//! (low, mid, high) as (red, green, blue), the brightest channel brought to full, and its height is
//! the mean peak of the whole signal.

use crate::fidlib::{Design, Kind, Runner};

/// Where the bands part: below `LOW_MID_HZ` is low, above `MID_HIGH_HZ` high.
const LOW_MID_HZ: f64 = 600.0;
const MID_HIGH_HZ: f64 = 4000.0;
/// The detail resolution of the analysis: one stride per 1/441 s.
const STRIDES_PER_SECOND: f64 = 441.0;
/// Strides are averaged into cells that double in length whenever this many have piled up, so
/// the memory does not grow with the length of a file and the average stays exact.
const MAX_CELLS: usize = 4096;

/// The three filters of one channel, side by side.
#[derive(Clone)]
struct Bands {
    low: Runner,
    mid: Runner,
    high: Runner,
}

impl Bands {
    fn new(rate: f64) -> Bands {
        Bands {
            low: Design::bessel4(Kind::LowPass, rate, LOW_MID_HZ, 0.0).runner(),
            mid: Design::bessel4(Kind::BandPass, rate, LOW_MID_HZ, MID_HIGH_HZ).runner(),
            high: Design::bessel4(Kind::HighPass, rate, MID_HIGH_HZ, 0.0).runner(),
        }
    }

    /// Low, mid and high part of `x`.
    fn split(&mut self, x: f64) -> [f64; 3] {
        [self.low.run(x), self.mid.run(x), self.high.run(x)]
    }
}

/// The sum of the peaks (low, mid, high and the whole signal) of some strides and how many there were.
pub(crate) type Cell = ([f64; 4], u32);

fn add(a: Cell, b: Cell) -> Cell {
    ([a.0[0] + b.0[0], a.0[1] + b.0[1], a.0[2] + b.0[2], a.0[3] + b.0[3]], a.1 + b.1)
}

/// Feeds frames through the filters and keeps the band peaks of the strides.
pub(crate) struct BandPeaks {
    channels: usize,
    filters: [Bands; 2],
    stride_frames: f64,
    position: f64,
    next_stride: f64,
    peaks: [f32; 4],
    started: bool,
    per_cell: u32,
    open: Cell,
    cells: Vec<Cell>,
}

impl BandPeaks {
    pub fn new(rate: u32, channels: usize) -> BandPeaks {
        let bands = Bands::new(f64::from(rate));
        let stride_frames = f64::from(rate) / STRIDES_PER_SECOND;
        BandPeaks { channels, filters: [bands.clone(), bands], stride_frames, position: 0.0, next_stride: stride_frames, peaks: [0.0; 4], started: false, per_cell: 1, open: ([0.0; 4], 0), cells: Vec::new() }
    }

    /// One frame of interleaved samples. More than two channels are folded to two (even ones to the
    /// left, odd ones to the right); one channel is analysed once.
    pub fn push(&mut self, frame: &[f32]) {
        let (left, right) = match frame {
            [only] => (f64::from(*only), 0.0),
            [l, r] => (f64::from(*l), f64::from(*r)),
            _ => {
                let fold = |first: usize| frame.iter().skip(first).step_by(2).map(|&s| f64::from(s)).sum::<f64>() / frame.len().div_ceil(2).max(1) as f64;
                (fold(0), fold(1))
            }
        };
        for (chain, x) in self.filters.iter_mut().zip([left, right]).take(self.channels.min(2)) {
            let [low, mid, high] = chain.split(x);
            for (peak, level) in self.peaks.iter_mut().zip([low, mid, high, x]) {
                *peak = peak.max(level.abs() as f32);
            }
        }
        self.started = true;
        self.position += 1.0;
        if self.position >= self.next_stride {
            self.end_stride();
            self.next_stride += self.stride_frames;
        }
    }

    fn end_stride(&mut self) {
        self.open = add(self.open, (self.peaks.map(f64::from), 1));
        self.peaks = [0.0; 4];
        self.started = false;
        if self.open.1 >= self.per_cell {
            self.cells.push(std::mem::take(&mut self.open));
            if self.cells.len() >= MAX_CELLS {
                self.cells = self.cells.chunks(2).map(|pair| pair.iter().copied().reduce(add).unwrap_or_default()).collect();
                self.per_cell *= 2;
            }
        }
    }

    /// The cells in order, the last stride and cell included even when they are short.
    pub fn finish(mut self) -> Vec<Cell> {
        if self.started {
            self.end_stride();
        }
        if self.open.1 > 0 {
            self.cells.push(self.open);
        }
        self.cells
    }
}

/// One colour for each of `n` equal runs of `cells`: the mean peak of each band over the run, as
/// red, green and blue with the brightest at 255. A silent run is grey. `cells` is cut the same way
/// as in [`heights`], so the two line up.
pub(crate) fn colors(cells: &[Cell], n: usize) -> Vec<[u8; 3]> {
    let m = cells.len();
    (0..n)
        .map(|b| {
            if m == 0 {
                return [128; 3];
            }
            let lo = (b * m / n).min(m - 1);
            let hi = ((b + 1) * m / n).clamp(lo + 1, m);
            let (sum, strides) = cells[lo..hi].iter().copied().reduce(add).unwrap_or_default();
            let mean = [sum[0], sum[1], sum[2]].map(|s| s / f64::from(strides.max(1)));
            let brightest = mean.iter().copied().fold(0.0, f64::max);
            if brightest <= 0.0 {
                return [128; 3];
            }
            mean.map(|v| (v / brightest * 255.0).round() as u8)
        })
        .collect()
}

/// The height of each of `n` equal runs of `cells`: the mean peak of the whole signal, 0 to 255.
pub(crate) fn heights(cells: &[Cell], n: usize) -> Vec<u8> {
    let m = cells.len();
    (0..n)
        .map(|b| {
            if m == 0 {
                return 0;
            }
            let lo = (b * m / n).min(m - 1);
            let hi = ((b + 1) * m / n).clamp(lo + 1, m);
            let (sum, strides) = cells[lo..hi].iter().copied().reduce(add).unwrap_or_default();
            (sum[3] / f64::from(strides.max(1)) * 255.0).round().min(255.0) as u8
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 44100.0;

    fn sine(hz: f64, i: usize) -> f64 {
        0.5 * (std::f64::consts::TAU * hz * i as f64 / RATE).sin()
    }

    #[test]
    fn the_mid_band_sits_between_the_corners() {
        // Bessel filters roll off gently: a tone an octave and a half from a corner still leaks about a tenth.
        let fresh = Bands::new(RATE);
        for (hz, band) in [(80.0, 0), (1500.0, 1), (10000.0, 2)] {
            let mut bands = fresh.clone();
            let loudest = (0..44100).map(|i| bands.split(sine(hz, i))).skip(22050).fold([0f64; 3], |m, s| [m[0].max(s[0].abs()), m[1].max(s[1].abs()), m[2].max(s[2].abs())]);
            let dominant = (0..3).max_by(|&x, &y| loudest[x].total_cmp(&loudest[y])).unwrap();
            assert_eq!(dominant, band, "{hz} Hz: {loudest:?}");
            let others = (0..3).filter(|&k| k != band).map(|k| loudest[k]).fold(0.0, f64::max);
            assert!(others < loudest[band] * 0.15, "{hz} Hz leaks: {loudest:?}");
        }
    }

    #[test]
    fn colours_are_the_band_peaks_as_red_green_and_blue() {
        let mut peaks = BandPeaks::new(44100, 1);
        for hz in [80.0, 1500.0, 10000.0] {
            for i in 0..22050 {
                peaks.push(&[sine(hz, i) as f32]);
            }
        }
        let cells = peaks.finish();
        let colors = colors(&cells, 3);
        for (c, band) in colors.iter().zip(0..3) {
            assert_eq!(c[band], 255, "{colors:?}");
            assert!(c.iter().enumerate().all(|(k, &v)| k == band || v < 40), "{colors:?}");
        }
    }

    #[test]
    fn strides_come_441_to_the_second_and_cells_keep_their_mean() {
        let mut peaks = BandPeaks::new(44100, 1);
        // Ten seconds is 4410 strides: past MAX_CELLS, so the cells have been merged once.
        for i in 0..441_000 {
            peaks.push(&[sine(1500.0, i) as f32]);
        }
        let cells = peaks.finish();
        assert!(cells.len() <= MAX_CELLS);
        assert_eq!(cells.iter().map(|c| c.1).sum::<u32>(), 4410);
        // A steady tone has the same mean in every cell, merged or not.
        let means: Vec<f64> = cells.iter().skip(2).map(|c| c.0[1] / f64::from(c.1)).collect();
        assert!(means.iter().all(|m| (m - means[0]).abs() < 0.01), "{means:?}");
    }

    #[test]
    fn heights_are_the_mean_peak_of_the_whole_signal() {
        let mut peaks = BandPeaks::new(44100, 2);
        // Quiet in the left channel, a sine at 0.5 in the right: the loudest channel counts.
        for i in 0..44100 {
            peaks.push(&[0.1, sine(1500.0, i) as f32]);
        }
        let h = heights(&peaks.finish(), 4);
        assert!(h.iter().all(|&v| (v as i32 - 128).abs() <= 2), "{h:?}");
        assert_eq!(heights(&[], 2), [0, 0]);
    }

    #[test]
    fn channels_are_folded_and_silence_is_grey() {
        let mut peaks = BandPeaks::new(44100, 6);
        for _ in 0..4410 {
            peaks.push(&[0.0; 6]);
        }
        assert_eq!(colors(&peaks.finish(), 2), [[128; 3]; 2]);
        assert_eq!(colors(&[], 2), [[128; 3]; 2]);
    }
}
