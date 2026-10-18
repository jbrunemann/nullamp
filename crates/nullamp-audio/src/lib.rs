//! Nullamp's audio engine.
//!
//! ```text
//!  controller ──commands──▶ engine thread ──samples──▶ ring buffer ──▶ cpal callback
//!      ▲                    (decode, seek,                              (gain ramp,
//!      └──────events─────── resample, device                            position)
//!                           recovery)
//! ```
//!
//! - Decoding and file IO happen on a dedicated engine thread, never in the audio
//!   callback, so slow disks or network volumes cannot cause glitches by themselves.
//! - Pause, resume and volume are applied in the callback through atomics: they take
//!   effect within one audio period without a round trip through the engine thread.
//! - Every loaded file is tagged with a caller-chosen `token`. Events and positions
//!   carry the token, so the caller can ignore anything that belongs to a track it
//!   has already moved away from.
//! - A preloaded next track is decoded into the same buffer for gapless transitions.

mod decode;
mod engine;
mod output;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender};

use crate::engine::Cmd;
use crate::output::Shared;

/// Properties of the decoded audio stream.
#[derive(Clone, Debug, PartialEq)]
pub struct StreamInfo {
    pub duration: Option<f64>,
    pub sample_rate: u32,
    pub channels: u16,
    pub codec: String,
    /// Average bitrate in kbit/s, estimated from file size and duration.
    pub bitrate: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum LoadError {
    #[error("file not found")]
    NotFound,
    #[error("cannot read file: {0}")]
    Io(String),
    #[error("cannot decode: {0}")]
    Unsupported(String),
    #[error("audio device unavailable: {0}")]
    Device(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum EngineEvent {
    /// A `load` succeeded. Playback starts (or waits, if loaded paused) immediately.
    Loaded { token: u64, info: StreamInfo },
    /// A `load` failed. Nothing is playing afterwards.
    LoadFailed { token: u64, error: LoadError },
    /// The preloaded track could not be opened. The current track will end normally.
    PreloadFailed { token: u64, error: LoadError },
    /// Playback crossed seamlessly into the preloaded track.
    TrackStarted { token: u64, info: StreamInfo },
    /// The track played to its end and no preloaded track followed.
    Finished { token: u64 },
    Seeked { token: u64, position: f64 },
    /// Decoding failed partway through. Buffered audio plays out, then `Finished`.
    PlaybackError { token: u64, message: String },
    DeviceLost { message: String },
    DeviceReady { name: String },
}

pub struct AudioEngine {
    tx: Sender<Cmd>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

/// Called after every event so the receiver can wake up (e.g. request a UI repaint).
pub type Waker = Arc<dyn Fn() + Send + Sync>;

impl AudioEngine {
    pub fn start(waker: Waker) -> (AudioEngine, Receiver<EngineEvent>) {
        let (tx, rx) = crossbeam_channel::unbounded();
        let (event_tx, event_rx) = crossbeam_channel::unbounded();
        let shared = Arc::new(Shared::default());
        shared.gain_bits.store(1.0f32.to_bits(), Ordering::Relaxed);
        let thread_shared = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("nullamp-audio".into())
            .spawn(move || engine::run(rx, event_tx, waker, thread_shared))
            .expect("spawn audio engine thread");
        (AudioEngine { tx, shared, thread: Some(thread) }, event_rx)
    }

    /// Stop whatever is playing and open `path`, starting at `start_at` seconds.
    pub fn load(&self, token: u64, path: PathBuf, start_at: f64, play: bool) {
        assert!(token != 0, "token 0 is reserved");
        self.shared.paused.store(!play, Ordering::Relaxed);
        let _ = self.tx.send(Cmd::Load { token, path, start_at, play });
    }

    /// Set (or clear) the track to continue with when the current one ends.
    pub fn preload(&self, next: Option<(u64, PathBuf)>) {
        let _ = self.tx.send(Cmd::Preload(next));
    }

    pub fn set_paused(&self, paused: bool) {
        // Applied by the callback right away; the command only manages the stream.
        self.shared.paused.store(paused, Ordering::Relaxed);
        let _ = self.tx.send(if paused { Cmd::Pause } else { Cmd::Play });
    }

    pub fn stop(&self) {
        let _ = self.tx.send(Cmd::Stop);
    }

    pub fn seek(&self, secs: f64) {
        if secs.is_finite() {
            let _ = self.tx.send(Cmd::Seek(secs.max(0.0)));
        }
    }

    /// Linear output gain, applied with a short ramp.
    pub fn set_gain(&self, gain: f32) {
        let gain = if gain.is_finite() { gain.clamp(0.0, 1.0) } else { 0.0 };
        self.shared.gain_bits.store(gain.to_bits(), Ordering::Relaxed);
    }

    /// The token of the audio at the playhead and its position in seconds.
    pub fn position(&self) -> (u64, f64) {
        let pos = f64::from_bits(self.shared.position_bits.load(Ordering::Acquire));
        (self.shared.position_token.load(Ordering::Acquire), pos)
    }

    /// Fill `out` with the mono signal (before volume) ending at the frame the listener
    /// hears `lead` from now, for visualisation. `lead` covers the display's own delay.
    /// Returns the sample rate, or `None` if nothing has been played yet. Lock-free.
    pub fn scope(&self, out: &mut [f32], lead: std::time::Duration) -> Option<u32> {
        self.shared.scope.read(out, lead)
    }

    /// Number of times the device asked for audio that was not ready yet.
    pub fn underruns(&self) -> u64 {
        self.shared.underruns.load(Ordering::Relaxed)
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Map a 0..1 volume control position to linear gain.
///
/// A cubic curve approximates perceived loudness well enough over a ~60 dB range,
/// so the slider feels even across its travel.
pub fn volume_to_gain(volume: f32) -> f32 {
    let v = volume.clamp(0.0, 1.0);
    v * v * v
}
