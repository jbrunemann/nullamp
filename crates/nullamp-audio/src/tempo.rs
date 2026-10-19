//! Tempo (BPM) estimation from a stretch of the file.
//!
//! The file is decoded at its own rate, mixed to mono and decimated to about 11 kHz, which is
//! plenty for onsets. The onset strength per hop is the positive spectral flux; its
//! autocorrelation peaks at the beat period and its multiples, and a comb of those peaks over
//! 60 to 200 BPM, weighted towards the common 120, picks the tempo. Runs on whatever thread
//! calls it and checks `cancel` between packets.
//!
//! Only about a minute and a half is analysed, from the middle of the track where the beat is
//! steadiest, so the cost stays a fraction of a second whatever the length. For a long mix that
//! is the tempo of its middle, which is only the mix's tempo if it keeps one; pass the span of
//! one of its tracks to get that track's instead.

use std::path::Path;
use std::sync::Arc;

use realfft::RealFftPlanner;

use crate::LoadError;
use crate::decode::{Decoding, Produced};

/// Analysis rate the decimation aims for; the actual rate is the file's divided by a whole number.
const TARGET_RATE: f64 = 11_025.0;
const FFT_SIZE: usize = 512;
const HOP: usize = 128;
/// Audio analysed, in seconds.
pub const WINDOW_SECONDS: f64 = 90.0;
/// Shorter stretches are too little to find a beat in.
const MIN_SECONDS: f64 = 10.0;
/// Tracks up to this long are analysed from their start; longer ones from [`START_FRACTION`] of the way in.
const SHORT_TRACK_SECONDS: f64 = 100.0;
const START_FRACTION: f64 = 0.3;
/// Share of a span that is analysed (from its middle), leaving out the transitions at either end.
const SPAN_CENTRE: f64 = 0.7;

const MIN_BPM: f64 = 60.0;
const MAX_BPM: f64 = 200.0;
const BPM_STEP: f64 = 0.1;
/// Tempo the octave decision leans towards: of half, double and so on, the nearest (in octaves) wins.
const PRIOR_BPM: f64 = 120.0;
/// Octaves from PRIOR_BPM at which the preference for it has faded to 61% when picking the dominant period.
const LEAN_OCTAVES: f64 = 1.25;
/// The comb sums the autocorrelation at this many multiples of the beat period, each weighted
/// by 1/k: the long lags pin the tempo down precisely, the short ones keep a tempo whose
/// period is a multiple of the true one (two thirds, say) from tying with it.
const COMB_MULTIPLES: usize = 8;
/// Normalised autocorrelation at the chosen period below which the beat is too weak to report.
const MIN_CONFIDENCE: f64 = 0.15;
/// Mean spectral flux below which the audio is silence or a steady tone, whatever the normalised autocorrelation says
/// (a click track measures 0.9, a steady tone 0.001).
const MIN_MEAN_FLUX: f32 = 0.05;
/// Estimates this close to a whole number are taken to be it: the estimate is good to about a quarter BPM.
const SNAP: f64 = 0.35;
/// Window of the running mean subtracted from the onset strength, in seconds.
const LOCAL_MEAN_SECONDS: f64 = 1.0;

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum TempoError {
    #[error(transparent)]
    Load(#[from] LoadError),
    #[error("decoding failed: {0}")]
    Decode(String),
    #[error("cancelled")]
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tempo {
    /// Beats per minute, whole when close to a whole number, else to one decimal.
    pub bpm: f32,
    /// How regular the beat is, 0 to 1. Steady electronic music scores 0.4 and up.
    pub confidence: f32,
}

/// Estimate the tempo of `path`, or of the part of it between `span` (start, end in seconds)
/// when given. `Ok(None)` means the audio has no clear beat, or that the span is too short.
pub fn compute(path: &Path, span: Option<(f64, f64)>, cancel: &dyn Fn() -> bool) -> Result<Option<Tempo>, TempoError> {
    let mut dec = Decoding::open_native(path)?;
    let rate = dec.info.sample_rate.max(1);
    let channels = usize::from(dec.info.channels).max(1);
    let (start, seconds) = match (span, dec.info.duration) {
        (Some((from, to)), _) => {
            let seconds = ((to - from) * SPAN_CENTRE).min(WINDOW_SECONDS);
            (Some(from + ((to - from) - seconds) / 2.0), seconds)
        }
        (None, Some(d)) if d > SHORT_TRACK_SECONDS => (Some(d * START_FRACTION), WINDOW_SECONDS),
        (None, _) => (None, WINDOW_SECONDS),
    };
    if seconds < MIN_SECONDS {
        return Ok(None);
    }
    if let Some(start) = start
        && let Err(e) = dec.seek(start)
    {
        log::debug!("tempo: cannot seek {}: {e}", path.display());
    }

    let factor = ((f64::from(rate) / TARGET_RATE).round() as usize).max(1);
    let analysis_rate = f64::from(rate) / factor as f64;
    let wanted = (seconds * analysis_rate) as usize;
    let mut mono: Vec<f32> = Vec::with_capacity(wanted);
    let (mut acc, mut n) = (0f32, 0usize);
    while mono.len() < wanted {
        if cancel() {
            return Err(TempoError::Cancelled);
        }
        match dec.produce().map_err(TempoError::Decode)? {
            Produced::Eof => break,
            Produced::Data => {
                for frame in dec.ready[dec.ready_pos..].chunks_exact(channels) {
                    acc += frame.iter().sum::<f32>() / channels as f32;
                    n += 1;
                    if n == factor {
                        mono.push(acc / factor as f32);
                        (acc, n) = (0.0, 0);
                    }
                }
                dec.ready_pos = dec.ready.len();
            }
        }
    }
    Ok(estimate(&mono, analysis_rate))
}

/// Tempo of mono `samples` at `rate` Hz, or `None` when too short or without a clear beat.
fn estimate(samples: &[f32], rate: f64) -> Option<Tempo> {
    if (samples.len() as f64) < MIN_SECONDS * rate {
        return None;
    }
    let onsets = onset_strength(samples);
    if onsets.iter().sum::<f32>() < MIN_MEAN_FLUX * onsets.len() as f32 {
        return None;
    }
    let env_rate = rate / HOP as f64;
    let acf = autocorrelation(&detrend(&onsets, env_rate), (COMB_MULTIPLES as f64 * env_rate * 60.0 / MIN_BPM) as usize + 2)?;

    let at = |lag: f64| {
        let i = lag.floor() as usize;
        match (acf.get(i), acf.get(i + 1)) {
            (Some(a), Some(b)) => a + (b - a) * (lag - i as f64),
            _ => 0.0,
        }
    };
    let comb = |bpm: f64| -> f64 {
        let period = env_rate * 60.0 / bpm;
        (1..=COMB_MULTIPLES).map(|k| at(period * k as f64) / k as f64).sum()
    };
    // The strongest periodicity. The lean towards PRIOR_BPM is too mild to decide between half and
    // double (a track whose odd beats are weak has its best period at two beats), only to break ties
    // between unrelated tempos.
    let steps = ((MAX_BPM - MIN_BPM) / BPM_STEP) as usize;
    let (mut best, mut dominant) = (f64::MIN, MIN_BPM);
    for bpm in (0..=steps).map(|i| MIN_BPM + i as f64 * BPM_STEP) {
        let lean = (bpm / PRIOR_BPM).log2() / LEAN_OCTAVES;
        let score = comb(bpm) * (-0.5 * lean * lean).exp();
        if score > best {
            (best, dominant) = (score, bpm);
        }
    }
    let confidence = at(env_rate * 60.0 / dominant);
    if confidence < MIN_CONFIDENCE {
        return None;
    }
    // Half and double the dominant tempo fit the audio equally well; take the one nearest PRIOR_BPM.
    let distance = |bpm: f64| (bpm / PRIOR_BPM).log2().abs();
    let mut octave = dominant;
    for bpm in (1..=4).flat_map(|n| [dominant * f64::from(1 << n), dominant / f64::from(1 << n)]) {
        if (MIN_BPM..=MAX_BPM).contains(&bpm) && distance(bpm) < distance(octave) {
            octave = bpm;
        }
    }
    // Halving or doubling the estimate multiplies its error, so fit again around the result.
    let refined = (-20..=20).map(|i| octave * (1.0 + f64::from(i) * 0.0005)).max_by(|a, b| comb(*a).total_cmp(&comb(*b))).unwrap_or(octave);
    let bpm = if (refined - refined.round()).abs() < SNAP { refined.round() } else { (refined * 10.0).round() / 10.0 };
    Some(Tempo { bpm: bpm as f32, confidence: confidence.min(1.0) as f32 })
}

/// Positive spectral flux per hop, on log-compressed magnitudes so quiet and loud passages count alike.
fn onset_strength(samples: &[f32]) -> Vec<f32> {
    let fft = RealFftPlanner::<f32>::new().plan_fft_forward(FFT_SIZE);
    let window: Vec<f32> = (0..FFT_SIZE).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / FFT_SIZE as f32).cos()).collect();
    let scale = 2.0 / window.iter().sum::<f32>();
    let (mut input, mut spectrum) = (fft.make_input_vec(), fft.make_output_vec());
    let mut prev = vec![0f32; spectrum.len()];
    let frames = (samples.len() - FFT_SIZE) / HOP + 1;
    let mut out = Vec::with_capacity(frames);
    for f in 0..frames {
        for ((x, w), s) in input.iter_mut().zip(&window).zip(&samples[f * HOP..]) {
            *x = w * s;
        }
        // The FFT only fails on mismatched buffer lengths, which `make_*_vec` rules out.
        let _ = fft.process(&mut input, &mut spectrum);
        let mut flux = 0.0;
        // Bin 0 is DC; skip it so a drifting offset is not an onset.
        for (p, c) in prev.iter_mut().zip(&spectrum).skip(1) {
            let level = (1.0 + 20.0 * c.norm() * scale).ln();
            flux += (level - *p).max(0.0);
            *p = level;
        }
        // The first frame has nothing to compare with.
        out.push(if f == 0 { 0.0 } else { flux });
    }
    out
}

/// Onset strength with its slow trend removed and negatives dropped, so what remains are the peaks, lightly smoothed.
fn detrend(onsets: &[f32], env_rate: f64) -> Vec<f32> {
    let half = ((LOCAL_MEAN_SECONDS * env_rate) as usize / 2).max(1);
    let mut prefix = Vec::with_capacity(onsets.len() + 1);
    prefix.push(0f64);
    for &o in onsets {
        prefix.push(prefix.last().copied().unwrap_or(0.0) + f64::from(o));
    }
    let out: Vec<f32> = (0..onsets.len())
        .map(|i| {
            let (lo, hi) = (i.saturating_sub(half), (i + half + 1).min(onsets.len()));
            let mean = (prefix[hi] - prefix[lo]) / (hi - lo) as f64;
            (f64::from(onsets[i]) - mean).max(0.0) as f32
        })
        .collect();
    // Spread each peak over a few hops: onsets land between hop boundaries, and without this the
    // autocorrelation is only high at whole-hop lags, so the comb misjudges fractional periods.
    let mut out: Vec<f32> = (0..out.len())
        .map(|i| {
            let tap = |d: isize, w: f32| out.get(i.wrapping_add_signed(d)).map_or(0.0, |v| v * w);
            (tap(-2, 1.0) + tap(-1, 2.0) + tap(0, 3.0) + tap(1, 2.0) + tap(2, 1.0)) / 9.0
        })
        .collect();
    let mean = out.iter().sum::<f32>() / out.len().max(1) as f32;
    out.iter_mut().for_each(|o| *o -= mean);
    out
}

/// Autocorrelation for lags `0..max_lag`, normalised so lag 0 is 1 and corrected for the
/// shrinking overlap. `None` for a flat signal.
fn autocorrelation(x: &[f32], max_lag: usize) -> Option<Arc<[f64]>> {
    let n = x.len();
    let max_lag = max_lag.min(n / 2);
    let energy: f64 = x.iter().map(|&v| f64::from(v) * f64::from(v)).sum();
    if max_lag < 2 || energy <= 1e-9 {
        return None;
    }
    let acf = (0..max_lag)
        .map(|lag| {
            let sum: f64 = x[..n - lag].iter().zip(&x[lag..]).map(|(&a, &b)| f64::from(a) * f64::from(b)).sum();
            sum / energy * n as f64 / (n - lag) as f64
        })
        .collect();
    Some(acf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures").join(name)
    }

    /// Four-on-the-floor: a kick on every beat and a short hat on the offbeat, over a quiet noise floor.
    fn click_track(bpm: f64, seconds: f64, rate: f64) -> Vec<f32> {
        accented_click_track(bpm, seconds, rate, 1.0)
    }

    /// As [`click_track`], with every second kick scaled by `odd_gain`.
    fn accented_click_track(bpm: f64, seconds: f64, rate: f64, odd_gain: f64) -> Vec<f32> {
        let beat = 60.0 / bpm;
        let mut seed = 12345u32;
        (0..(seconds * rate) as usize)
            .map(|i| {
                let t = i as f64 / rate;
                let gain = if (t / beat) as u64 % 2 == 1 { odd_gain } else { 1.0 };
                let kick = 0.8 * gain * (std::f64::consts::TAU * 55.0 * t).sin() * (-25.0 * (t % beat)).exp();
                let hat_t = (t + beat / 2.0) % beat;
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = f64::from(seed >> 16) / 32768.0 - 1.0;
                (kick + 0.15 * noise * (-120.0 * hat_t).exp() + 0.003 * noise) as f32
            })
            .collect()
    }

    #[test]
    fn finds_the_tempo_of_click_tracks() {
        for bpm in [85.0, 90.0, 100.0, 110.0, 120.0, 128.0, 140.0, 150.0, 160.0] {
            let t = estimate(&click_track(bpm, 60.0, 11_025.0), 11_025.0).unwrap_or_else(|| panic!("no tempo for {bpm}"));
            assert!((f64::from(t.bpm) - bpm).abs() < 0.5, "{bpm} BPM came out as {}", t.bpm);
            assert!(t.confidence > 0.3, "{bpm}: confidence {}", t.confidence);
        }
    }

    #[test]
    fn weak_odd_beats_do_not_halve_the_tempo() {
        // The strongest periodicity is two beats; the answer is still the beat.
        for bpm in [118.0, 126.0, 135.0] {
            let t = estimate(&accented_click_track(bpm, 60.0, 11_025.0, 0.3), 11_025.0).unwrap();
            assert!((f64::from(t.bpm) - bpm).abs() < 0.5, "{bpm} BPM came out as {}", t.bpm);
        }
    }

    #[test]
    fn finds_fractional_tempos() {
        let t = estimate(&click_track(127.5, 60.0, 11_025.0), 11_025.0).unwrap();
        assert!((f64::from(t.bpm) - 127.5).abs() < 0.3, "{}", t.bpm);
    }

    #[test]
    fn steady_tones_and_silence_have_no_tempo() {
        let tone: Vec<f32> = (0..11_025 * 30).map(|i| (i as f32 * 0.1).sin() * 0.5).collect();
        assert_eq!(estimate(&tone, 11_025.0), None);
        assert_eq!(estimate(&vec![0.0; 11_025 * 30], 11_025.0), None);
    }

    #[test]
    fn short_audio_is_not_analysed() {
        assert_eq!(estimate(&click_track(120.0, 5.0, 11_025.0), 11_025.0), None);
    }

    #[test]
    fn decodes_and_decimates_a_file() {
        // 22.05 kHz mono mp3 of kicks at 120 BPM.
        let t = compute(&fixture("beat_120.mp3"), None, &|| false).unwrap().expect("tempo");
        assert!((t.bpm - 120.0).abs() < 0.5, "{}", t.bpm);
    }

    #[test]
    fn a_span_is_analysed_on_its_own() {
        // 15 s at 120 BPM, then 15 s at 90 BPM.
        let tempo = |span| compute(&fixture("beats_120_90.mp3"), Some(span), &|| false).unwrap().map(|t| t.bpm);
        assert!((tempo((0.0, 15.0)).unwrap() - 120.0).abs() < 0.5);
        assert!((tempo((15.0, 30.0)).unwrap() - 90.0).abs() < 0.5);
        assert_eq!(tempo((2.0, 10.0)), None, "too short to tell");
    }

    #[test]
    fn tones_have_no_tempo_and_errors_are_reported() {
        assert_eq!(compute(&fixture("tone.flac"), None, &|| false), Ok(None));
        assert_eq!(compute(&fixture("beat_120.mp3"), None, &|| true), Err(TempoError::Cancelled));
        assert_eq!(compute(Path::new("/nonexistent/x.mp3"), None, &|| false), Err(TempoError::Load(LoadError::NotFound)));
    }
}
