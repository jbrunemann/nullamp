//! The audio device side: a cpal output stream reading from a ring buffer.
//!
//! The real-time callback never blocks. It takes the playhead with `try_lock`; the
//! engine thread only holds that lock briefly to flush the buffer or register a track
//! boundary. If the lock is contended the callback plays silence for one period.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use crossbeam_channel::Sender;

use crate::kind::{OutputKind, kind_of};

/// Seconds of decoded audio held in the ring buffer.
const RING_SECONDS: f64 = 2.0;
/// Device period asked for on Linux. With `BufferSize::Default`, cpal keeps two of the periods
/// the ALSA device picks, and through the PulseAudio or PipeWire plugins that can add up to
/// seconds: a seek or track change was only heard about 2 s later (issue #6). Two 40 ms periods
/// keep that delay under a tenth of a second and still leave room against underruns.
#[cfg(target_os = "linux")]
const LINUX_PERIOD_SECONDS: f64 = 0.04;
/// Length of the fade applied to pause, resume and volume changes.
const RAMP_SECONDS: f32 = 0.015;

/// State shared lock-free between the callback, the engine thread and the UI.
#[derive(Default)]
pub(crate) struct Shared {
    pub position_bits: AtomicU64,
    pub position_token: AtomicU64,
    pub paused: AtomicBool,
    pub gain_bits: AtomicU32,
    /// Set by the callback when the stream for this token has played out. 0 = none.
    pub ended_token: AtomicU64,
    /// Set by the callback when it crosses into a preloaded track. 0 = none.
    pub started_token: AtomicU64,
    /// Callback invocation counter, used as a watchdog.
    pub callbacks: AtomicU64,
    pub underruns: AtomicU64,
    pub scope: Scope,
}

impl Shared {
    pub fn publish_position(&self, token: u64, secs: f64) {
        self.position_token.store(token, Ordering::Release);
        self.position_bits.store(secs.to_bits(), Ordering::Release);
    }

    pub fn gain(&self) -> f32 {
        f32::from_bits(self.gain_bits.load(Ordering::Relaxed))
    }
}

/// Mono frames kept for visualisation (a power of two; about 170 ms at 48 kHz).
pub(crate) const SCOPE_LEN: usize = 8192;

/// A lock-free history of what the callback sent to the device, for visualisation.
///
/// The callback writes a whole period ahead of the speaker, so it also records when the
/// first frame of that period is due to be heard. Readers use that to pick the frame
/// that is audible right now instead of the newest one, which keeps the picture in
/// step with the sound rather than a buffer ahead of it.
pub(crate) struct Scope {
    samples: Box<[AtomicU32]>,
    /// Total frames written.
    written: AtomicU64,
    /// Frame index at the start of the latest period and when it reaches the speaker.
    period_start: AtomicU64,
    period_due_ns: AtomicU64,
    rate: AtomicU32,
    epoch: Instant,
}

impl Default for Scope {
    fn default() -> Self {
        Scope {
            samples: (0..SCOPE_LEN).map(|_| AtomicU32::new(0)).collect(),
            written: AtomicU64::new(0),
            period_start: AtomicU64::new(0),
            period_due_ns: AtomicU64::new(0),
            rate: AtomicU32::new(0),
            epoch: Instant::now(),
        }
    }
}

impl Scope {
    /// Called by the callback with one period of mono frames, `latency` before they play.
    pub fn write(&self, frames: &[f32], rate: u32, latency: std::time::Duration) {
        let start = self.written.load(Ordering::Relaxed);
        for (i, s) in frames.iter().enumerate() {
            self.samples[(start as usize + i) % SCOPE_LEN].store(s.to_bits(), Ordering::Relaxed);
        }
        let due = (self.epoch.elapsed() + latency).as_nanos() as u64;
        self.rate.store(rate, Ordering::Relaxed);
        self.period_due_ns.store(due, Ordering::Relaxed);
        self.period_start.store(start, Ordering::Relaxed);
        self.written.store(start + frames.len() as u64, Ordering::Release);
    }

    /// Fill `out` with the frames ending at the one audible `lead` from now.
    /// Returns the sample rate, or `None` before anything was played.
    pub fn read(&self, out: &mut [f32], lead: std::time::Duration) -> Option<u32> {
        let written = self.written.load(Ordering::Acquire);
        let rate = self.rate.load(Ordering::Relaxed);
        if written == 0 || rate == 0 || out.len() > SCOPE_LEN / 2 {
            return None;
        }
        let start = self.period_start.load(Ordering::Relaxed) as i64;
        let due = self.period_due_ns.load(Ordering::Relaxed) as i64;
        let now = (self.epoch.elapsed() + lead).as_nanos() as i64;
        let audible = start + (now - due) * rate as i64 / 1_000_000_000;
        // Stay inside the history; the oldest half may be overwritten while we read.
        let end = audible.clamp(written.saturating_sub(SCOPE_LEN as u64 / 2) as i64, written as i64) as u64;
        let first = end.saturating_sub(out.len() as u64);
        let pad = out.len() - (end - first) as usize;
        out[..pad].fill(0.0);
        for (i, o) in out[pad..].iter_mut().enumerate() {
            *o = f32::from_bits(self.samples[(first as usize + i) % SCOPE_LEN].load(Ordering::Relaxed));
        }
        Some(rate)
    }
}

/// A track boundary inside the ring buffer (gapless transition).
pub(crate) struct Switch {
    pub at_total: u64,
    pub token: u64,
}

pub(crate) struct Playhead {
    pub consumer: rtrb::Consumer<f32>,
    /// Track whose samples are at the read position.
    pub token: u64,
    /// Position in seconds of the first frame after the last flush or switch.
    pub base: f64,
    /// Frames played since `base`.
    pub frames: u64,
    /// Frames consumed since the last flush.
    pub total: u64,
    pub switches: VecDeque<Switch>,
    /// Total frame count at which the data ends, once the decoder has finished.
    pub end_at: Option<u64>,
    pub ended: bool,
    /// Playback waits for an initial amount of buffered audio after a flush.
    pub primed: bool,
}

pub(crate) struct Output {
    stream: cpal::Stream,
    pub producer: rtrb::Producer<f32>,
    pub playhead: Arc<Mutex<Playhead>>,
    pub rate: u32,
    pub channels: usize,
    pub device_name: String,
    pub device_id: String,
    pub kind: OutputKind,
    /// A specific device was requested but is not available, so the system default is used.
    pub fell_back: bool,
    /// Frames pushed since the last flush (engine-side counterpart of `Playhead::total`).
    pub pushed_total: u64,
    stream_running: bool,
}

#[derive(Debug)]
pub(crate) enum StreamEvent {
    /// The device went away or changed; the stream must be rebuilt.
    Invalidated(String),
}

/// An audio output the user can choose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputDevice {
    /// Stable identifier to persist and pass back to the engine.
    pub id: String,
    pub name: String,
    pub is_default: bool,
    pub kind: OutputKind,
}

/// Every output device the system currently offers, in the host's order.
pub(crate) fn list_output_devices() -> Vec<OutputDevice> {
    let host = cpal::default_host();
    let default_id = host.default_output_device().and_then(|d| d.id().ok());
    let Ok(devices) = host.output_devices() else { return Vec::new() };
    devices
        .filter_map(|d| {
            let id = d.id().ok()?;
            Some(OutputDevice { name: d.to_string(), is_default: default_id.as_ref() == Some(&id), kind: kind_of(&id, &d), id: id.to_string() })
        })
        .collect()
}

/// The device with this id, if it is present and can play audio.
fn find_device(host: &cpal::Host, id: &str) -> Option<cpal::Device> {
    let id: cpal::DeviceId = id.parse().ok()?;
    host.device_by_id(&id).filter(|d| d.supports_output())
}

impl Output {
    /// Open `preferred` (an id from [`list_output_devices`]) or, without one or when it
    /// is gone, the system default output.
    pub fn open(shared: Arc<Shared>, events: Sender<StreamEvent>, preferred: Option<&str>) -> Result<Output, String> {
        let host = cpal::default_host();
        let chosen = preferred.and_then(|id| find_device(&host, id));
        let fell_back = preferred.is_some() && chosen.is_none();
        let device = match chosen {
            Some(d) => d,
            None => host.default_output_device().ok_or("no audio output device")?,
        };
        let device_name = device.to_string();
        let id = device.id().ok();
        let device_id = id.as_ref().map(|i| i.to_string()).unwrap_or_default();
        let kind = id.as_ref().map_or(OutputKind::Unknown, |id| kind_of(id, &device));
        let supported = device.default_output_config().map_err(|e| format!("output config: {e}"))?;
        let format = supported.sample_format();
        let buffer_size = preferred_buffer_size(&supported);
        let mut config: cpal::StreamConfig = supported.into();
        config.buffer_size = buffer_size;
        let rate = config.sample_rate;
        let channels = config.channels as usize;
        if channels == 0 || rate == 0 {
            return Err("device reports no channels".into());
        }

        let capacity = (rate as f64 * RING_SECONDS) as usize * channels;
        let (producer, consumer) = rtrb::RingBuffer::new(capacity);
        let playhead = Arc::new(Mutex::new(Playhead {
            consumer,
            token: 0,
            base: 0.0,
            frames: 0,
            total: 0,
            switches: VecDeque::with_capacity(8),
            end_at: None,
            ended: false,
            primed: false,
        }));

        let stream = match build_any(format, &device, config, &playhead, &shared, &events) {
            // A device that rejects the requested period still plays with its own default.
            Err(e) if config.buffer_size != cpal::BufferSize::Default => {
                log::warn!("audio output: {:?} frame periods rejected ({e}); using the device default", config.buffer_size);
                config.buffer_size = cpal::BufferSize::Default;
                build_any(format, &device, config, &playhead, &shared, &events)
            }
            r => r,
        }?;
        log::info!("audio output: {device_name}, {rate} Hz, {channels} ch, {format}, buffer {:?}", config.buffer_size);
        Ok(Output { stream, producer, playhead, rate, channels, device_name, device_id, kind, fell_back, pushed_total: 0, stream_running: false })
    }

    pub fn set_running(&mut self, running: bool) -> Result<(), String> {
        if running == self.stream_running {
            return Ok(());
        }
        let r = if running { self.stream.play() } else { self.stream.pause() };
        r.map_err(|e| e.to_string())?;
        self.stream_running = running;
        Ok(())
    }

    pub fn is_running(&self) -> bool {
        self.stream_running
    }

    /// Discard all buffered audio and restart position tracking at `base` for `token`.
    pub fn flush(&mut self, token: u64, base: f64, shared: &Shared) {
        let mut ph = self.playhead.lock().unwrap_or_else(|e| e.into_inner());
        let n = ph.consumer.slots();
        if let Ok(chunk) = ph.consumer.read_chunk(n) {
            chunk.commit_all();
        }
        ph.token = token;
        ph.base = base;
        ph.frames = 0;
        ph.total = 0;
        ph.switches.clear();
        ph.end_at = None;
        ph.ended = false;
        ph.primed = false;
        self.pushed_total = 0;
        shared.ended_token.store(0, Ordering::Release);
        shared.started_token.store(0, Ordering::Release);
        shared.publish_position(token, base);
    }

    /// Push as many samples from `src` as fit. Returns the number of samples written.
    pub fn push(&mut self, src: &[f32]) -> usize {
        let n = self.producer.slots().min(src.len());
        let n = n - n % self.channels;
        if n == 0 {
            return 0;
        }
        if let Ok(mut chunk) = self.producer.write_chunk(n) {
            let (a, b) = chunk.as_mut_slices();
            a.copy_from_slice(&src[..a.len()]);
            b.copy_from_slice(&src[a.len()..n]);
            chunk.commit_all();
        }
        self.pushed_total += (n / self.channels) as u64;
        n
    }

    pub fn free_frames(&self) -> usize {
        self.producer.slots() / self.channels
    }

    pub fn buffered_frames(&self) -> usize {
        self.capacity_frames() - self.free_frames()
    }

    pub fn capacity_frames(&self) -> usize {
        self.producer.buffer().capacity() / self.channels
    }

    pub fn with_playhead<R>(&self, f: impl FnOnce(&mut Playhead) -> R) -> R {
        let mut ph = self.playhead.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut ph)
    }
}

/// The device period to request: the system's own default except on Linux, see [`LINUX_PERIOD_SECONDS`].
#[cfg(target_os = "linux")]
fn preferred_buffer_size(supported: &cpal::SupportedStreamConfig) -> cpal::BufferSize {
    let want = (supported.sample_rate() as f64 * LINUX_PERIOD_SECONDS) as u32;
    match *supported.buffer_size() {
        cpal::SupportedBufferSize::Range { min, max } => cpal::BufferSize::Fixed(want.clamp(min, max.max(min))),
        cpal::SupportedBufferSize::Unknown => cpal::BufferSize::Fixed(want),
    }
}

#[cfg(not(target_os = "linux"))]
fn preferred_buffer_size(_supported: &cpal::SupportedStreamConfig) -> cpal::BufferSize {
    cpal::BufferSize::Default
}

fn build_any(
    format: SampleFormat,
    device: &cpal::Device,
    config: cpal::StreamConfig,
    playhead: &Arc<Mutex<Playhead>>,
    shared: &Arc<Shared>,
    events: &Sender<StreamEvent>,
) -> Result<cpal::Stream, String> {
    match format {
        SampleFormat::F32 => build::<f32>(device, config, playhead, shared, events),
        SampleFormat::F64 => build::<f64>(device, config, playhead, shared, events),
        SampleFormat::I16 => build::<i16>(device, config, playhead, shared, events),
        SampleFormat::I32 => build::<i32>(device, config, playhead, shared, events),
        SampleFormat::U16 => build::<u16>(device, config, playhead, shared, events),
        SampleFormat::I24 => build::<cpal::I24>(device, config, playhead, shared, events),
        SampleFormat::U8 => build::<u8>(device, config, playhead, shared, events),
        SampleFormat::I8 => build::<i8>(device, config, playhead, shared, events),
        other => Err(format!("unsupported device sample format {other}")),
    }
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    playhead: &Arc<Mutex<Playhead>>,
    shared: &Arc<Shared>,
    events: &Sender<StreamEvent>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let rate = config.sample_rate as f32;
    let ramp_step = 1.0 / (RAMP_SECONDS * rate);
    let playhead = Arc::clone(playhead);
    let shared_cb = Arc::clone(shared);
    let mut scratch: Vec<f32> = Vec::with_capacity(8192 * channels);
    let mut tap: Vec<f32> = Vec::with_capacity(8192);
    let mut gain = 0.0f32;

    let data_cb = move |data: &mut [T], info: &cpal::OutputCallbackInfo| {
        shared_cb.callbacks.fetch_add(1, Ordering::Relaxed);
        if scratch.len() < data.len() {
            scratch.resize(data.len(), 0.0);
        }
        let frames = data.len() / channels;
        if tap.len() < frames {
            tap.resize(frames, 0.0);
        }
        let out = &mut scratch[..data.len()];
        let tap = &mut tap[..frames];
        out.fill(0.0);
        tap.fill(0.0);
        if let Ok(mut ph) = playhead.try_lock() {
            render(&mut ph, out, tap, channels, rate, ramp_step, &mut gain, &shared_cb);
        }
        for (d, s) in data.iter_mut().zip(out.iter()) {
            *d = T::from_sample(*s);
        }
        let ts = info.timestamp();
        shared_cb.scope.write(tap, rate as u32, ts.playback.duration_since(ts.callback));
    };
    let events = events.clone();
    let err_cb = move |err: cpal::Error| match err.kind() {
        cpal::ErrorKind::Xrun => log::debug!("audio xrun"),
        cpal::ErrorKind::RealtimeDenied => log::debug!("real-time scheduling denied"),
        _ => {
            log::warn!("audio stream error: {err}");
            let _ = events.send(StreamEvent::Invalidated(err.to_string()));
        }
    };
    device.build_output_stream(config, data_cb, err_cb, None).map_err(|e| format!("open output stream: {e}"))
}

/// Fill `out` (interleaved f32, already zeroed) from the playhead, and `tap` (one
/// mono frame per output frame, already zeroed) with the same audio before volume, so
/// the visualisation does not shrink with the volume control.
#[allow(clippy::too_many_arguments)]
fn render(ph: &mut Playhead, out: &mut [f32], tap: &mut [f32], ch: usize, rate: f32, ramp_step: f32, gain: &mut f32, shared: &Shared) {
    let mono_scale = 1.0 / ch as f32;
    let paused = shared.paused.load(Ordering::Relaxed);
    let target = if paused { 0.0 } else { shared.gain() };
    let wanted = out.len() / ch;
    let mut written = 0usize;

    if ph.primed {
        while written < wanted {
            if paused && *gain <= 0.0 {
                break;
            }
            while let Some(sw) = ph.switches.front() {
                if sw.at_total > ph.total {
                    break;
                }
                ph.token = sw.token;
                ph.base = 0.0;
                ph.frames = 0;
                shared.started_token.store(sw.token, Ordering::Release);
                ph.switches.pop_front();
            }
            if let Some(end) = ph.end_at
                && ph.total >= end
            {
                if !ph.ended {
                    ph.ended = true;
                    shared.ended_token.store(ph.token, Ordering::Release);
                }
                break;
            }
            let mut limit = (wanted - written) as u64;
            if let Some(sw) = ph.switches.front() {
                limit = limit.min(sw.at_total - ph.total);
            }
            if let Some(end) = ph.end_at {
                limit = limit.min(end - ph.total);
            }
            if paused {
                // Consume only what the fade-out needs, so resuming continues exactly here.
                limit = limit.min(((*gain / ramp_step).ceil() as u64).max(1));
            }
            let available = (ph.consumer.slots() / ch) as u64;
            let n = limit.min(available) as usize;
            if n == 0 {
                shared.underruns.fetch_add(1, Ordering::Relaxed);
                break;
            }
            let Ok(chunk) = ph.consumer.read_chunk(n * ch) else { break };
            let (a, b) = chunk.as_slices();
            let dst = &mut out[written * ch..(written + n) * ch];
            let tap_dst = &mut tap[written..written + n];
            for ((frame_dst, frame_src), t) in dst.chunks_exact_mut(ch).zip(a.chunks_exact(ch).chain(b.chunks_exact(ch))).zip(tap_dst) {
                if *gain != target {
                    *gain = if *gain < target { (*gain + ramp_step).min(target) } else { (*gain - ramp_step).max(target) };
                }
                let mut sum = 0.0;
                for (d, s) in frame_dst.iter_mut().zip(frame_src) {
                    *d = s * *gain;
                    sum += s;
                }
                *t = sum * mono_scale;
            }
            chunk.commit_all();
            written += n;
            ph.total += n as u64;
            ph.frames += n as u64;
        }
    }
    shared.publish_position(ph.token, ph.base + ph.frames as f64 / rate as f64);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playhead(samples: &[f32]) -> Playhead {
        let (mut p, c) = rtrb::RingBuffer::new(1024);
        for s in samples {
            p.push(*s).unwrap();
        }
        // Producer dropped: the consumer can still read what was pushed.
        Playhead {
            consumer: c,
            token: 1,
            base: 10.0,
            frames: 0,
            total: 0,
            switches: VecDeque::new(),
            end_at: None,
            ended: false,
            primed: true,
        }
    }

    fn tap(samples: usize) -> Vec<f32> {
        vec![0.0; samples / 2]
    }

    fn shared(gain: f32) -> Shared {
        let s = Shared::default();
        s.gain_bits.store(gain.to_bits(), Ordering::Relaxed);
        s
    }

    #[test]
    fn unknown_device_ids_are_not_found() {
        let host = cpal::default_host();
        assert!(find_device(&host, "not an id").is_none());
        assert!(find_device(&host, "coreaudio:no-such-device").is_none());
    }

    #[test]
    fn listed_devices_have_unique_ids_and_at_most_one_default() {
        let devices = list_output_devices();
        let mut ids: Vec<_> = devices.iter().map(|d| &d.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), devices.len());
        assert!(devices.iter().filter(|d| d.is_default).count() <= 1);
    }

    #[test]
    fn renders_and_tracks_position() {
        let mut ph = playhead(&[1.0; 200]);
        let sh = shared(1.0);
        let mut gain = 1.0;
        let mut out = vec![0.0; 100];
        let mut t = tap(out.len());
        render(&mut ph, &mut out, &mut t, 2, 100.0, 1.0, &mut gain, &sh);
        assert!(out.iter().all(|s| *s == 1.0));
        assert_eq!(ph.total, 50);
        let pos = f64::from_bits(sh.position_bits.load(Ordering::Relaxed));
        assert!((pos - 10.5).abs() < 1e-9);
    }

    #[test]
    fn not_primed_outputs_silence_without_consuming() {
        let mut ph = playhead(&[1.0; 20]);
        ph.primed = false;
        let sh = shared(1.0);
        let mut out = vec![0.0; 10];
        let mut t = tap(out.len());
        render(&mut ph, &mut out, &mut t, 2, 100.0, 1.0, &mut 1.0, &sh);
        assert!(out.iter().all(|s| *s == 0.0));
        assert_eq!(ph.consumer.slots(), 20);
    }

    #[test]
    fn switch_and_end_markers() {
        let mut ph = playhead(&[0.5; 20]);
        ph.switches.push_back(Switch { at_total: 4, token: 2 });
        ph.end_at = Some(8);
        let sh = shared(1.0);
        let mut out = vec![0.0; 20];
        let mut t = tap(out.len());
        render(&mut ph, &mut out, &mut t, 2, 100.0, 1.0, &mut 1.0, &sh);
        assert_eq!(&out[..16], &[0.5; 16]);
        assert_eq!(&out[16..], &[0.0; 4]);
        assert_eq!(ph.token, 2);
        assert_eq!(sh.started_token.load(Ordering::Relaxed), 2);
        assert_eq!(sh.ended_token.load(Ordering::Relaxed), 2);
        let pos = f64::from_bits(sh.position_bits.load(Ordering::Relaxed));
        assert!((pos - 0.04).abs() < 1e-9, "position restarts at the switch: {pos}");
    }

    #[test]
    fn pause_fades_out_then_stops_consuming() {
        let mut ph = playhead(&[1.0; 400]);
        let sh = shared(1.0);
        sh.paused.store(true, Ordering::Relaxed);
        let mut gain = 1.0;
        let mut out = vec![0.0; 400];
        // ramp_step 0.25: gain reaches 0 after four frames.
        let mut t = tap(out.len());
        render(&mut ph, &mut out, &mut t, 2, 100.0, 0.25, &mut gain, &sh);
        assert_eq!(&out[..8], &[0.75, 0.75, 0.5, 0.5, 0.25, 0.25, 0.0, 0.0]);
        assert_eq!(gain, 0.0);
        let consumed = ph.total;
        let mut out2 = vec![0.0; 40];
        let mut t = tap(out2.len());
        render(&mut ph, &mut out2, &mut t, 2, 100.0, 0.25, &mut gain, &sh);
        assert_eq!(ph.total, consumed, "nothing consumed while paused");
    }

    #[test]
    fn underrun_is_counted_and_silent() {
        let mut ph = playhead(&[1.0; 4]);
        let sh = shared(1.0);
        let mut out = vec![0.0; 10];
        let mut t = tap(out.len());
        render(&mut ph, &mut out, &mut t, 2, 100.0, 1.0, &mut 1.0, &sh);
        assert_eq!(&out[..4], &[1.0; 4]);
        assert_eq!(&out[4..], &[0.0; 6]);
        assert_eq!(sh.underruns.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn tap_is_mono_and_ignores_volume() {
        let mut ph = playhead(&[0.2, 0.6, 0.2, 0.6]);
        let sh = shared(0.5);
        let mut out = vec![0.0; 4];
        let mut tap = vec![0.0; 2];
        render(&mut ph, &mut out, &mut tap, 2, 100.0, 1.0, &mut 0.5, &sh);
        assert_eq!(out, [0.1, 0.3, 0.1, 0.3]);
        assert!(tap.iter().all(|t| (t - 0.4).abs() < 1e-6), "{tap:?}");
    }

    #[test]
    fn scope_reads_the_audible_frame_not_the_newest() {
        let scope = Scope::default();
        let mut out = [9.0; 4];
        assert_eq!(scope.read(&mut out, std::time::Duration::ZERO), None);
        let frames: Vec<f32> = (0..1000).map(|i| i as f32).collect();
        // One second of latency at 1 kHz: nothing of this period is audible yet.
        scope.write(&frames, 1000, std::time::Duration::from_secs(1));
        assert_eq!(scope.read(&mut out, std::time::Duration::ZERO), Some(1000));
        assert_eq!(out, [0.0; 4]);
        // Looking 1.5 s ahead lands about halfway into the period.
        scope.read(&mut out, std::time::Duration::from_millis(1500));
        assert!((495.0..=510.0).contains(&out[3]), "{out:?}");
        assert_eq!(out[3] - out[0], 3.0);
        // Far ahead clamps to the newest frame.
        scope.read(&mut out, std::time::Duration::from_secs(10));
        assert_eq!(out, [996.0, 997.0, 998.0, 999.0]);
    }
}
