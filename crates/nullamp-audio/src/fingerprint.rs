//! A Chromaprint fingerprint of the start of a file, in the form AcoustID takes.
//!
//! Decodes at the file's own rate and channel layout and feeds the first
//! [`MAX_SECONDS`] to the fingerprinter, which resamples and downmixes internally. Uses
//! the same algorithm and length cap as `fpcalc`, so the output matches what that tool prints.

use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rusty_chromaprint::{Configuration, FingerprintCompressor, Fingerprinter};

use crate::LoadError;
use crate::decode::{Decoding, Produced};

/// Audio fed to the fingerprinter; `fpcalc` uses the same default.
pub const MAX_SECONDS: u32 = 120;

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum FingerprintError {
    #[error(transparent)]
    Load(#[from] LoadError),
    #[error("decoding failed: {0}")]
    Decode(String),
    #[error("no audio in file")]
    Empty,
    #[error("fingerprinting failed: {0}")]
    Fingerprint(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Fingerprint {
    /// Length of the whole file in whole seconds, as AcoustID's `duration` parameter wants it.
    pub duration: u32,
    /// Seconds of audio the fingerprint covers (at most [`MAX_SECONDS`]).
    pub covered: f64,
    /// Compressed and URL-safe base64 encoded, AcoustID's `fingerprint` parameter.
    pub text: String,
}

/// Decode `path` and fingerprint its first [`MAX_SECONDS`].
pub fn compute(path: &Path) -> Result<Fingerprint, FingerprintError> {
    let mut dec = Decoding::open_native(path)?;
    let rate = dec.info.sample_rate.max(1);
    let channels = usize::from(dec.info.channels).max(1);
    let limit = u64::from(rate) * u64::from(MAX_SECONDS);

    let config = Configuration::preset_test2();
    let mut printer = Fingerprinter::new(&config);
    printer.start(rate, channels as u32).map_err(|e| FingerprintError::Fingerprint(e.to_string()))?;

    let (mut fed, mut total) = (0u64, 0u64);
    let mut pcm: Vec<i16> = Vec::new();
    loop {
        // Past the cap only the length is still needed, and only when the container did not state it.
        if fed >= limit && dec.info.duration.is_some() {
            break;
        }
        match dec.produce().map_err(FingerprintError::Decode)? {
            Produced::Eof => break,
            Produced::Data => {
                let new = &dec.ready[dec.ready_pos..];
                let frames = (new.len() / channels) as u64;
                total += frames;
                let take = frames.min(limit - fed) as usize * channels;
                if take > 0 {
                    pcm.clear();
                    pcm.extend(new[..take].iter().map(|s| (s.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16));
                    printer.consume(&pcm);
                    fed += (take / channels) as u64;
                }
                dec.ready_pos = dec.ready.len();
            }
        }
    }
    if fed == 0 {
        return Err(FingerprintError::Empty);
    }
    printer.finish();

    let raw = printer.fingerprint();
    let text = URL_SAFE_NO_PAD.encode(FingerprintCompressor::from(&config).compress(raw));
    let duration = dec.info.duration.unwrap_or(total as f64 / f64::from(rate)).round() as u32;
    Ok(Fingerprint { duration, covered: fed as f64 / f64::from(rate), text })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures").join(name)
    }

    #[test]
    fn fingerprints_a_fixture_deterministically() {
        let path = fixture("mix_chapters.mp3");
        let a = compute(&path).unwrap();
        let b = compute(&path).unwrap();
        assert_eq!(a, b);
        assert!(a.text.starts_with("AQAA"), "algorithm 2 header, got {}", a.text);
        assert!(a.duration > 0 && a.covered <= f64::from(MAX_SECONDS));
    }

    #[test]
    fn missing_file_is_a_load_error() {
        assert_eq!(compute(Path::new("/nonexistent/x.mp3")), Err(FingerprintError::Load(LoadError::NotFound)));
    }
}
