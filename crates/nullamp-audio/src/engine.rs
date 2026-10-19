//! The engine thread: owns the decoder and the output stream.
//!
//! Opening a file (probing, reading headers) happens on short-lived loader threads,
//! because it can take seconds on a cold disk or a network volume. The engine thread
//! keeps handling commands meanwhile; results for requests that were superseded are
//! dropped.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};

use crate::decode::{Decoding, Produced};
use crate::output::{Output, Shared, StreamEvent, Switch};
use crate::{EngineEvent, LoadError, StreamInfo, Waker};

pub(crate) enum Cmd {
    Load { token: u64, path: PathBuf, start_at: f64, play: bool },
    Preload(Option<(u64, PathBuf)>),
    Play,
    Pause,
    Stop,
    Seek(f64),
    SetDevice(Option<String>),
    ListDevices,
    Shutdown,
}

/// Audio buffered after a flush before playback (re)starts.
const PRIME_SECONDS: f64 = 0.12;
/// Stop the device stream after being idle this long, so the system can sleep.
const IDLE_STREAM_TIMEOUT: Duration = Duration::from_secs(3);
/// Treat the device as dead if the callback stops running while playing.
const WATCHDOG: Duration = Duration::from_millis(2000);
const DEVICE_RETRY: Duration = Duration::from_secs(1);
/// When the decoder reaches the end while the next track is still being opened, wait
/// this long for it (a short gap) before ending the track instead.
const PRELOAD_GRACE: Duration = Duration::from_secs(2);
/// Upper bound on decoding work between checks for new commands.
const FILL_BUDGET: Duration = Duration::from_millis(20);

struct Track {
    token: u64,
    path: PathBuf,
    info: StreamInfo,
}

/// Result of a loader thread.
struct Opened {
    token: u64,
    rate: u32,
    channels: usize,
    /// Position the decoder was seeked to.
    base: f64,
    result: Result<Decoding, LoadError>,
}

struct PendingLoad {
    token: u64,
    path: PathBuf,
    start_at: f64,
    play: bool,
}

enum Preloaded {
    Opening(u64),
    Ready(Box<Decoding>),
    Failed,
}

struct Engine {
    events: Sender<EngineEvent>,
    waker: Waker,
    shared: Arc<Shared>,
    stream_tx: Sender<StreamEvent>,
    stream_rx: Receiver<StreamEvent>,
    opened_tx: Sender<Opened>,
    opened_rx: Receiver<Opened>,
    output: Option<Output>,
    /// The device the user chose; `None` follows the system default.
    preferred: Option<String>,
    device_lost: bool,
    retry_at: Option<Instant>,
    pending_load: Option<PendingLoad>,
    /// The track being decoded into the ring (may be ahead of the playhead).
    decoding: Option<Decoding>,
    /// The track at the playhead.
    head: Option<Track>,
    /// Tracks decoded ahead whose boundary the playhead has not reached yet.
    upcoming: Vec<Track>,
    next: Option<(u64, PathBuf)>,
    preloaded: Option<Preloaded>,
    primed: bool,
    active: bool,
    paused: bool,
    idle_since: Option<Instant>,
    watchdog: (u64, Instant),
    /// When the decoder hit the end while waiting for the preload to open.
    eof_since: Option<Instant>,
}

pub(crate) fn run(rx: Receiver<Cmd>, events: Sender<EngineEvent>, waker: Waker, shared: Arc<Shared>, preferred: Option<String>) {
    let (stream_tx, stream_rx) = crossbeam_channel::unbounded();
    let (opened_tx, opened_rx) = crossbeam_channel::unbounded();
    let mut e = Engine {
        events,
        waker,
        shared,
        stream_tx,
        stream_rx,
        opened_tx,
        opened_rx,
        output: None,
        preferred,
        device_lost: false,
        retry_at: None,
        pending_load: None,
        decoding: None,
        head: None,
        upcoming: Vec::new(),
        next: None,
        preloaded: None,
        primed: false,
        active: false,
        paused: true,
        idle_since: Some(Instant::now()),
        watchdog: (0, Instant::now()),
        eof_since: None,
    };
    // Open the device early so the first play is fast.
    e.ensure_output();

    loop {
        let busy = e.wants_data() || !e.opened_rx.is_empty();
        let timeout = if busy { Duration::ZERO } else { Duration::from_millis(5) };
        let first = match rx.recv_timeout(timeout) {
            Ok(cmd) => Some(cmd),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if let Some(first) = first {
            let mut cmds = vec![first];
            cmds.extend(rx.try_iter());
            if !e.handle_all(cmds) {
                break;
            }
        }
        e.poll_opened();
        e.poll_stream();
        e.fill(&rx);
        e.housekeeping();
    }
    e.output = None;
}

impl Engine {
    fn emit(&self, event: EngineEvent) {
        let _ = self.events.send(event);
        (self.waker)();
    }

    fn handle_all(&mut self, cmds: Vec<Cmd>) -> bool {
        // A seek superseded by a later seek or load is skipped (e.g. while dragging).
        let last_reposition = cmds.iter().rposition(|c| matches!(c, Cmd::Seek(_) | Cmd::Load { .. }));
        for (i, cmd) in cmds.into_iter().enumerate() {
            if matches!(cmd, Cmd::Seek(_)) && last_reposition.is_some_and(|l| l > i) {
                continue;
            }
            if !self.handle(cmd) {
                return false;
            }
        }
        true
    }

    fn handle(&mut self, cmd: Cmd) -> bool {
        match cmd {
            Cmd::Load { token, path, start_at, play } => self.load(token, path, start_at, play),
            Cmd::Preload(next) => self.set_next(next),
            Cmd::Play => {
                self.paused = false;
                if self.active {
                    self.run_stream(true);
                }
            }
            Cmd::Pause => self.paused = true,
            Cmd::Stop => self.stop(),
            Cmd::Seek(secs) => self.seek(secs),
            Cmd::SetDevice(device) => self.set_device(device),
            Cmd::ListDevices => self.emit(EngineEvent::OutputDevices(crate::output::list_output_devices())),
            Cmd::Shutdown => return false,
        }
        true
    }

    fn ensure_output(&mut self) -> bool {
        if self.output.is_some() {
            return true;
        }
        match Output::open(Arc::clone(&self.shared), self.stream_tx.clone(), self.preferred.as_deref()) {
            Ok(out) => {
                let name = out.device_name.clone();
                let opened = EngineEvent::OutputOpened {
                    name: name.clone(),
                    id: out.device_id.clone(),
                    kind: out.kind,
                    sample_rate: out.rate,
                    channels: out.channels as u16,
                    fell_back: out.fell_back,
                };
                self.output = Some(out);
                self.emit(opened);
                self.retry_at = None;
                self.watchdog = (self.shared.callbacks.load(Ordering::Relaxed), Instant::now());
                if self.device_lost {
                    self.device_lost = false;
                    self.emit(EngineEvent::DeviceReady { name });
                }
                true
            }
            Err(msg) => {
                if !self.device_lost {
                    self.device_lost = true;
                    self.emit(EngineEvent::DeviceLost { message: msg });
                }
                self.retry_at = Some(Instant::now() + DEVICE_RETRY);
                false
            }
        }
    }

    fn set_device(&mut self, device: Option<String>) {
        if self.preferred == device {
            return;
        }
        self.preferred = device;
        self.rebuild_output("output device chosen".into());
    }

    fn run_stream(&mut self, running: bool) {
        if let Some(out) = self.output.as_mut()
            && let Err(e) = out.set_running(running)
        {
            log::warn!("cannot {} stream: {e}", if running { "start" } else { "pause" });
            if running {
                self.rebuild_output(e);
            }
        }
        if running {
            self.idle_since = None;
            self.watchdog = (self.shared.callbacks.load(Ordering::Relaxed), Instant::now());
        }
    }

    /// Open `path` on a loader thread. The result arrives through `opened_rx`.
    fn spawn_open(&self, token: u64, path: PathBuf, start_at: f64) -> bool {
        let Some(out) = self.output.as_ref() else { return false };
        let (rate, channels) = (out.rate, out.channels);
        let tx = self.opened_tx.clone();
        let spawned = std::thread::Builder::new().name("nullamp-open".into()).spawn(move || {
            let mut base = 0.0;
            let result = Decoding::open(token, &path, rate, channels).map(|mut d| {
                if start_at > 0.0 {
                    base = d.seek(start_at).unwrap_or(0.0);
                }
                d
            });
            // The engine loop polls at least every 5 ms, so no explicit wake-up is needed.
            let _ = tx.send(Opened { token, rate, channels, base, result });
        });
        spawned.is_ok()
    }

    fn load(&mut self, token: u64, path: PathBuf, start_at: f64, play: bool) {
        // Silence the previous track right away; the new one starts when opened.
        self.clear_playback();
        self.paused = !play;
        if !self.ensure_output() {
            let msg = "no output device".to_string();
            self.emit(EngineEvent::LoadFailed { token, error: LoadError::Device(msg) });
            return;
        }
        // Reuse a preloaded decoder for this very file if it is ready.
        if let Some(Preloaded::Ready(dec)) = &self.preloaded
            && dec.token == token
            && start_at <= 0.0
        {
            let Some(Preloaded::Ready(dec)) = self.preloaded.take() else { unreachable!() };
            self.next = None;
            self.install(*dec, path, 0.0, play);
            return;
        }
        self.pending_load = Some(PendingLoad { token, path: path.clone(), start_at, play });
        if !self.spawn_open(token, path, start_at) {
            self.pending_load = None;
            self.emit(EngineEvent::LoadFailed { token, error: LoadError::Io("cannot start loader".into()) });
        }
    }

    fn install(&mut self, dec: Decoding, path: PathBuf, base: f64, play: bool) {
        let Some(out) = self.output.as_mut() else { return };
        let token = dec.token;
        out.flush(token, base, &self.shared);
        let info = dec.info.clone();
        self.head = Some(Track { token, path, info: info.clone() });
        self.decoding = Some(dec);
        self.active = true;
        self.paused = !play;
        if play {
            self.run_stream(true);
        }
        self.emit(EngineEvent::Loaded { token, info });
    }

    fn poll_opened(&mut self) {
        while let Ok(opened) = self.opened_rx.try_recv() {
            let rate_ok = self.output.as_ref().is_some_and(|o| o.rate == opened.rate && o.channels == opened.channels);
            if self.pending_load.as_ref().is_some_and(|p| p.token == opened.token) {
                let p = self.pending_load.take().unwrap();
                match opened.result {
                    Ok(_) if !rate_ok => {
                        // The device changed while opening: open again for the new format.
                        self.load(p.token, p.path, p.start_at, p.play);
                    }
                    Ok(dec) => self.install(dec, p.path, opened.base, p.play),
                    Err(error) => self.emit(EngineEvent::LoadFailed { token: opened.token, error }),
                }
            } else if matches!(self.preloaded, Some(Preloaded::Opening(t)) if t == opened.token) {
                self.preloaded = match opened.result {
                    Ok(dec) if rate_ok => Some(Preloaded::Ready(Box::new(dec))),
                    Ok(_) => None,
                    Err(error) => {
                        self.emit(EngineEvent::PreloadFailed { token: opened.token, error });
                        Some(Preloaded::Failed)
                    }
                };
            }
            // Anything else is a stale result for a superseded request: dropped here.
        }
    }

    fn clear_playback(&mut self) {
        self.eof_since = None;
        self.pending_load = None;
        self.decoding = None;
        self.head = None;
        self.upcoming.clear();
        self.active = false;
        self.primed = false;
        if let Some(out) = self.output.as_mut() {
            out.flush(0, 0.0, &self.shared);
        }
    }

    fn stop(&mut self) {
        self.clear_playback();
        self.next = None;
        self.preloaded = None;
        self.idle_since = Some(Instant::now());
    }

    fn set_next(&mut self, next: Option<(u64, PathBuf)>) {
        // Already decoding ahead into a track that is no longer wanted: end the current
        // track at the boundary instead. The caller loads its new choice on `Finished`.
        if let Some(pending) = self.upcoming.first()
            && next.as_ref().is_none_or(|(t, _)| *t != pending.token)
        {
            if let Some(out) = self.output.as_ref() {
                out.with_playhead(|ph| {
                    if let Some(sw) = ph.switches.front() {
                        ph.end_at = Some(sw.at_total);
                        ph.switches.clear();
                    }
                });
            }
            self.upcoming.clear();
            self.decoding = None;
        }
        if self.next == next {
            return;
        }
        self.next = next;
        self.preloaded = None;
        if let Some((token, path)) = self.next.clone()
            && self.spawn_open(token, path, 0.0)
        {
            self.preloaded = Some(Preloaded::Opening(token));
        }
    }

    fn seek(&mut self, secs: f64) {
        if let Some(p) = self.pending_load.as_mut() {
            // Still opening: start there instead.
            let (token, path, play) = (p.token, p.path.clone(), p.play);
            self.load(token, path, secs, play);
            return;
        }
        let Some(head) = self.head.as_ref() else { return };
        let token = head.token;
        // The decoder may be ahead in a preloaded track; seeking always targets the head.
        let reuse = self.decoding.as_ref().is_some_and(|d| d.token == token);
        if !reuse {
            let Some(out) = self.output.as_ref() else { return };
            match Decoding::open(token, &head.path, out.rate, out.channels) {
                Ok(d) => self.decoding = Some(d),
                Err(e) => {
                    self.emit(EngineEvent::PlaybackError { token, message: e.to_string() });
                    return;
                }
            }
            // The decoder that ran ahead was the preloaded track; it must be reopened.
            if self.next.is_some() {
                let next = self.next.take();
                self.set_next(next);
            }
        }
        let dec = self.decoding.as_mut().unwrap();
        match dec.seek(secs) {
            Ok(actual) => {
                self.upcoming.clear();
                self.primed = false;
                if let Some(out) = self.output.as_mut() {
                    out.flush(token, actual, &self.shared);
                }
                self.emit(EngineEvent::Seeked { token, position: actual });
            }
            Err(message) => self.emit(EngineEvent::PlaybackError { token, message }),
        }
    }

    fn wants_data(&self) -> bool {
        match (&self.decoding, &self.output) {
            (Some(d), _) if d.eof && d.ready_pos >= d.ready.len() && self.awaiting_preload() => {
                self.eof_since.is_some_and(|t| t.elapsed() >= PRELOAD_GRACE)
            }
            (Some(_), Some(out)) => out.free_frames() * 4 >= out.capacity_frames(),
            _ => false,
        }
    }

    /// The next track is requested and still being opened.
    fn awaiting_preload(&self) -> bool {
        matches!((&self.next, &self.preloaded), (Some((t, _)), Some(Preloaded::Opening(o))) if t == o)
    }

    /// Decode and push audio until the ring is full, the budget is spent, or a
    /// command is waiting.
    fn fill(&mut self, rx: &Receiver<Cmd>) {
        let started = Instant::now();
        loop {
            if !rx.is_empty() || !self.stream_rx.is_empty() || started.elapsed() > FILL_BUDGET {
                return;
            }
            let (Some(out), Some(dec)) = (self.output.as_mut(), self.decoding.as_mut()) else { return };

            if dec.ready_pos < dec.ready.len() {
                dec.ready_pos += out.push(&dec.ready[dec.ready_pos..]);
                let ring_full = dec.ready_pos < dec.ready.len();
                if !self.primed && (ring_full || out.buffered_frames() as f64 >= out.rate as f64 * PRIME_SECONDS) {
                    self.prime();
                }
                if ring_full {
                    return;
                }
                continue;
            }

            if dec.eof {
                if self.awaiting_preload() && started.duration_since(*self.eof_since.get_or_insert(started)) < PRELOAD_GRACE {
                    return;
                }
                self.eof_since = None;
                self.decoder_finished();
                continue;
            }

            match dec.produce() {
                Ok(Produced::Data) | Ok(Produced::Eof) => {}
                Err(message) => {
                    let token = dec.token;
                    log::warn!("decode error in {}: {message}", dec.path.display());
                    dec.eof = true;
                    self.emit(EngineEvent::PlaybackError { token, message });
                }
            }
        }
    }

    fn prime(&mut self) {
        self.primed = true;
        if let Some(out) = self.output.as_ref() {
            out.with_playhead(|ph| ph.primed = true);
        }
    }

    /// The decoder has pushed everything. Continue with the preloaded track if it is
    /// ready, or mark the end of the stream.
    fn decoder_finished(&mut self) {
        let Some(out) = self.output.as_mut() else { return };
        let at_total = out.pushed_total;
        let ready = match (&self.next, &self.preloaded) {
            (Some((t, _)), Some(Preloaded::Ready(d))) => d.token == *t,
            _ => false,
        };
        if ready {
            let Some(Preloaded::Ready(dec)) = self.preloaded.take() else { unreachable!() };
            let (token, path) = self.next.take().unwrap();
            out.with_playhead(|ph| ph.switches.push_back(Switch { at_total, token }));
            self.upcoming.push(Track { token, path, info: dec.info.clone() });
            self.decoding = Some(*dec);
            return;
        }
        // No usable preload (none requested, failed, or still opening on a slow
        // volume): end here and let the caller load the next track.
        out.with_playhead(|ph| ph.end_at = Some(at_total));
        self.decoding = None;
        if !self.primed {
            self.prime();
        }
    }

    fn poll_stream(&mut self) {
        let started = self.shared.started_token.swap(0, Ordering::AcqRel);
        if started != 0
            && let Some(i) = self.upcoming.iter().position(|t| t.token == started)
        {
            let track = self.upcoming.remove(i);
            let info = track.info.clone();
            self.head = Some(track);
            self.emit(EngineEvent::TrackStarted { token: started, info });
        }
        let ended = self.shared.ended_token.swap(0, Ordering::AcqRel);
        if ended != 0 && self.head.as_ref().is_some_and(|h| h.token == ended) {
            self.head = None;
            self.active = false;
            self.idle_since = Some(Instant::now());
            self.emit(EngineEvent::Finished { token: ended });
        }
        let mut invalidated = None;
        for ev in self.stream_rx.try_iter() {
            let StreamEvent::Invalidated(msg) = ev;
            invalidated = Some(msg);
        }
        if let Some(msg) = invalidated {
            self.rebuild_output(msg);
        }
    }

    fn housekeeping(&mut self) {
        let now = Instant::now();
        if self.output.is_none() && self.retry_at.is_some_and(|t| now >= t) {
            self.rebuild_output("retrying".into());
            return;
        }
        let Some(out) = self.output.as_mut() else { return };

        // Release the device while idle (paused or stopped).
        let idle = !self.active || self.paused;
        if idle {
            let since = *self.idle_since.get_or_insert(now);
            if out.is_running() && now.duration_since(since) > IDLE_STREAM_TIMEOUT {
                let _ = out.set_running(false);
            }
        } else {
            self.idle_since = None;
            if !out.is_running() {
                self.run_stream(true);
                return;
            }
            // Watchdog: a running stream whose callback stopped firing is dead.
            let calls = self.shared.callbacks.load(Ordering::Relaxed);
            if calls != self.watchdog.0 {
                self.watchdog = (calls, now);
            } else if now.duration_since(self.watchdog.1) > WATCHDOG {
                self.rebuild_output("audio device stopped responding".into());
            }
        }
    }

    /// Recreate the output stream (device changed, lost or stalled) and resume the
    /// track at the playhead from its current position.
    fn rebuild_output(&mut self, reason: String) {
        log::warn!("rebuilding audio output: {reason}");
        let (pos_token, pos) = (
            self.shared.position_token.load(Ordering::Acquire),
            f64::from_bits(self.shared.position_bits.load(Ordering::Acquire)),
        );
        let was_running = self.output.as_ref().is_some_and(|o| o.is_running());
        self.output = None;
        self.decoding = None;
        self.upcoming.clear();
        self.preloaded = None;
        self.primed = false;
        if !self.ensure_output() {
            return;
        }
        // The preload was opened for the old device format.
        if let Some(next) = self.next.take() {
            self.set_next(Some(next));
        }
        if let Some(p) = self.pending_load.take() {
            self.load(p.token, p.path, p.start_at, p.play);
            return;
        }
        let Some(head) = self.head.take() else { return };
        let resume_at = if pos_token == head.token { pos } else { 0.0 };
        let play = self.active && (!self.paused || was_running);
        self.pending_load = Some(PendingLoad { token: head.token, path: head.path.clone(), start_at: resume_at, play });
        // Keep reporting the old position until the reopened track is installed.
        self.shared.publish_position(head.token, resume_at);
        if !self.spawn_open(head.token, head.path, resume_at) {
            self.pending_load = None;
        }
        self.active = false;
    }
}
