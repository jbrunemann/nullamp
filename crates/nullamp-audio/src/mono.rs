//! Short clips of a file decoded to mono at a chosen rate, for recognising the tracks of a mix.
//!
//! Unlike the tempo pass, which decimates by a whole number from the file's own rate, this
//! resamples to exactly `rate`: a recognition service compares spectral peaks, which only
//! mean the same frequencies when the rate is the one it expects.

use std::path::Path;

use crate::LoadError;
use crate::decode::{Decoding, Produced};

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum MonoError {
    #[error(transparent)]
    Load(#[from] LoadError),
    #[error("decoding failed: {0}")]
    Decode(String),
    #[error("cancelled")]
    Cancelled,
}

/// `seconds` of `path` from each of `starts`, mixed to mono and resampled to `rate` Hz.
/// Each clip is reached by seeking, so a long mix is not decoded whole for a few clips.
/// A clip that runs past the end is shorter. `cancel` is asked before each clip.
pub fn decode_clips(path: &Path, rate: u32, starts: &[f64], seconds: f64, cancel: &dyn Fn() -> bool) -> Result<Vec<Vec<f32>>, MonoError> {
    let mut dec = Decoding::open(0, path, rate, 1)?;
    let want = (seconds * f64::from(rate)) as usize;
    let mut clips = Vec::with_capacity(starts.len());
    for &start in starts {
        if cancel() {
            return Err(MonoError::Cancelled);
        }
        dec.seek(start).map_err(MonoError::Decode)?;
        let mut clip = Vec::with_capacity(want);
        while clip.len() < want {
            let produced = dec.produce().map_err(MonoError::Decode)?;
            let take = (want - clip.len()).min(dec.ready.len() - dec.ready_pos);
            clip.extend_from_slice(&dec.ready[dec.ready_pos..dec.ready_pos + take]);
            dec.ready_pos += take;
            if let Produced::Eof = produced {
                break;
            }
        }
        clips.push(clip);
    }
    Ok(clips)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures").join(name)
    }

    #[test]
    fn decodes_mono_and_stereo_files_to_the_asked_rate() {
        for name in ["tone_mono_22k.wav", "tone.flac"] {
            // One clip longer than the file: all of it.
            let whole = decode_clips(&fixture(name), 8000, &[0.0], 10.0, &|| false).unwrap().remove(0);
            // Two seconds, give or take the resampler's delay.
            assert!((whole.len() as i64 - 16_000).abs() < 400, "{name}: {} samples", whole.len());
            assert!(whole.iter().any(|s| s.abs() > 0.05), "{name} is silent");
        }
    }

    #[test]
    fn clips_match_the_same_stretch_of_the_whole_decode() {
        let path = fixture("tone.flac");
        let whole = decode_clips(&path, 8000, &[0.0], 10.0, &|| false).unwrap().remove(0);
        let clips = decode_clips(&path, 8000, &[0.5, 1.5, 0.25], 0.4, &|| false).unwrap();
        assert_eq!(clips.iter().map(Vec::len).collect::<Vec<_>>(), [3200, 3200, 3200]);
        // The last clip asks for audio before the previous one, so the seek goes backwards.
        for (clip, start) in clips.iter().zip([0.5, 1.5, 0.25]) {
            let from = (start * 8000.0) as usize;
            let energy = |s: &[f32]| s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32;
            let (a, b) = (energy(clip), energy(&whole[from..from + 3200]));
            assert!((a - b).abs() < 0.2 * b, "clip at {start}: energy {a} against {b}");
        }
        // Past the end the clip is cut short.
        let tail = decode_clips(&path, 8000, &[1.9], 0.4, &|| false).unwrap();
        assert!(tail[0].len() < 3200, "{} samples", tail[0].len());
        assert_eq!(decode_clips(&path, 8000, &[0.0], 0.4, &|| true), Err(MonoError::Cancelled));
    }

    #[test]
    fn missing_file_is_a_load_error() {
        assert_eq!(decode_clips(Path::new("/nonexistent/x.mp3"), 8000, &[0.0], 1.0, &|| false), Err(MonoError::Load(LoadError::NotFound)));
    }
}
