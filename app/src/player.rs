//! The playback controller: turns commands and engine events into a coherent state.
//!
//! All transport commands (buttons, shortcuts, media keys, OS overlays) and all engine
//! events are handled here, on one thread, in arrival order. The engine is driven by
//! tokens: every load gets a fresh token, and events or positions for any other token
//! are ignored. That is what keeps rapid track changes, seeks and edits from
//! applying stale results to the wrong track.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;

use nullamp_audio::{AudioEngine, EngineEvent, LoadError, StreamInfo};
use nullamp_core::command::{Command, CommandSource, Deduplicator};
use nullamp_core::{EntryId, EntryStatus, Navigator, Playlist, PlaylistEntry, RepeatMode};

use crate::store::PlaylistDoc;

/// The engine operations the controller needs (implemented by `AudioEngine`, and by
/// a recording fake in tests).
pub trait Engine {
    fn load(&self, token: u64, path: PathBuf, start_at: f64, play: bool);
    fn preload(&self, next: Option<(u64, PathBuf)>);
    fn set_paused(&self, paused: bool);
    fn stop(&self);
    fn seek(&self, secs: f64);
    fn set_gain(&self, gain: f32);
    fn position(&self) -> (u64, f64);
}

impl Engine for AudioEngine {
    fn load(&self, token: u64, path: PathBuf, start_at: f64, play: bool) {
        AudioEngine::load(self, token, path, start_at, play)
    }
    fn preload(&self, next: Option<(u64, PathBuf)>) {
        AudioEngine::preload(self, next)
    }
    fn set_paused(&self, paused: bool) {
        AudioEngine::set_paused(self, paused)
    }
    fn stop(&self) {
        AudioEngine::stop(self)
    }
    fn seek(&self, secs: f64) {
        AudioEngine::seek(self, secs)
    }
    fn set_gain(&self, gain: f32) {
        AudioEngine::set_gain(self, gain)
    }
    fn position(&self) -> (u64, f64) {
        AudioEngine::position(self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Stopped,
    /// A file is being opened; playback starts when it is ready.
    Loading,
    Playing,
    Paused,
}

/// What is playing: which list, which entry, under which engine token.
#[derive(Clone, Debug, PartialEq)]
pub struct Current {
    pub list: usize,
    pub entry: EntryId,
    pub path: PathBuf,
    pub token: u64,
    pub info: Option<StreamInfo>,
}

/// Something the UI should react to (load metadata, update the OS, show a message).
#[derive(Clone, Debug, PartialEq)]
pub enum Note {
    /// A different file became current.
    TrackChanged,
    /// Play state or position changed discontinuously (for the OS media overlay).
    StateChanged,
    /// Playlist contents changed and should be saved.
    PlaylistsChanged,
    Error(String),
    Info(String),
}

const RESTART_THRESHOLD: f64 = 3.0;

pub struct Player<E: Engine> {
    pub engine: E,
    pub lists: Vec<PlaylistDoc>,
    /// The list shown in the playlist editor.
    pub viewing: usize,
    pub nav: Navigator,
    pub status: Status,
    pub current: Option<Current>,
    pub volume: f32,
    pub muted: bool,
    next_token: u64,
    preload: Option<(u64, usize, EntryId)>,
    /// Consecutive load failures while advancing automatically.
    failures: usize,
    /// Position to report while a load or seek is in flight.
    pub(crate) pending_position: Option<f64>,
    /// The in-flight load was started paused (skip while paused, seek while stopped).
    load_paused: bool,
    dedup: Deduplicator,
    pub notes: Vec<Note>,
}

impl<E: Engine> Player<E> {
    pub fn new(engine: E, lists: Vec<PlaylistDoc>, seed: u64) -> Self {
        let lists = if lists.is_empty() {
            vec![PlaylistDoc { key: "default".into(), playlist: Playlist::new("Default") }]
        } else {
            lists
        };
        Player {
            engine,
            lists,
            viewing: 0,
            nav: Navigator::new(seed),
            status: Status::Stopped,
            current: None,
            volume: 0.8,
            muted: false,
            next_token: 1,
            preload: None,
            failures: 0,
            pending_position: None,
            load_paused: false,
            dedup: Deduplicator::default(),
            notes: Vec::new(),
        }
    }

    #[cfg(test)]
    pub fn playlist(&self, list: usize) -> &Playlist {
        &self.lists[list].playlist
    }

    pub fn viewed(&self) -> &Playlist {
        &self.lists[self.viewing].playlist
    }

    /// The list that owns the current entry (or the viewed list when idle).
    pub fn playing_list(&self) -> usize {
        self.current.as_ref().map(|c| c.list).unwrap_or(self.viewing)
    }

    pub fn current_entry(&self) -> Option<&PlaylistEntry> {
        let c = self.current.as_ref()?;
        self.lists.get(c.list)?.playlist.get(c.entry)
    }

    pub fn is_current(&self, list: usize, entry: EntryId) -> bool {
        self.current.as_ref().is_some_and(|c| c.list == list && c.entry == entry)
    }

    /// Playback position in seconds for the current track.
    pub fn position(&self) -> f64 {
        let Some(c) = &self.current else { return 0.0 };
        if let Some(p) = self.pending_position {
            return p;
        }
        let (token, pos) = self.engine.position();
        if token == c.token { pos } else { 0.0 }
    }

    pub fn duration(&self) -> Option<f64> {
        let c = self.current.as_ref()?;
        c.info.as_ref().and_then(|i| i.duration).or_else(|| self.current_entry().and_then(|e| e.info.duration))
    }

    fn token(&mut self) -> u64 {
        let t = self.next_token;
        self.next_token += 1;
        t
    }

    fn apply_gain(&self) {
        let gain = if self.muted { 0.0 } else { nullamp_audio::volume_to_gain(self.volume) };
        self.engine.set_gain(gain);
    }

    pub fn set_volume(&mut self, v: f32) {
        self.volume = v.clamp(0.0, 1.0);
        self.apply_gain();
    }

    pub fn set_muted(&mut self, m: bool) {
        self.muted = m;
        self.apply_gain();
    }

    pub fn set_shuffle(&mut self, on: bool) {
        let list = self.playing_list();
        let current = self.current.as_ref().map(|c| c.entry);
        self.nav.set_shuffle(on, &self.lists[list].playlist, current);
        self.refresh_preload();
    }

    pub fn set_repeat(&mut self, mode: RepeatMode) {
        self.nav.repeat = mode;
        self.refresh_preload();
    }

    /// Start playing an entry from the beginning (or `start_at`).
    pub fn play_entry(&mut self, list: usize, entry: EntryId, start_at: f64) {
        self.failures = 0;
        self.start(list, entry, start_at, true);
    }

    fn start(&mut self, list: usize, entry: EntryId, start_at: f64, play: bool) {
        let Some(e) = self.lists.get(list).and_then(|l| l.playlist.get(entry)) else { return };
        let path = e.path.clone();
        // Reuse the preload token for the same entry so the engine can use the file it
        // already opened.
        let token = match self.preload {
            Some((t, l, id)) if l == list && id == entry && start_at <= 0.0 => t,
            _ => self.token(),
        };
        self.preload = None;
        // Playing again after a stop counts as a change: the deck dropped the file's waveform and cover.
        let changed = self.status == Status::Stopped || self.current.as_ref().is_none_or(|c| c.path != path || c.list != list || c.entry != entry);
        self.current = Some(Current { list, entry, path: path.clone(), token, info: None });
        self.status = Status::Loading;
        self.load_paused = !play;
        self.pending_position = Some(start_at.max(0.0));
        self.engine.load(token, path, start_at, play);
        self.nav.sync(&self.lists[list].playlist, Some(entry));
        if changed {
            self.notes.push(Note::TrackChanged);
        }
        self.notes.push(Note::StateChanged);
    }

    /// Handle a transport command from any source.
    pub fn command(&mut self, cmd: Command, source: CommandSource) {
        if !self.dedup.accept(cmd, source, Instant::now()) {
            return;
        }
        match cmd {
            Command::PlayPause => match self.status {
                Status::Playing | Status::Loading => self.pause(),
                Status::Paused => self.resume(),
                Status::Stopped => self.play_from_idle(),
            },
            Command::Play => match self.status {
                Status::Paused => self.resume(),
                Status::Stopped => self.play_from_idle(),
                _ => {}
            },
            Command::Pause => {
                if matches!(self.status, Status::Playing | Status::Loading) {
                    self.pause()
                }
            }
            Command::Stop => self.stop(),
            Command::Next => self.skip(true),
            Command::Previous => {
                if self.position() > RESTART_THRESHOLD && self.current.is_some() {
                    self.seek_to(0.0);
                } else {
                    self.skip(false);
                }
            }
            Command::SeekTo(t) => self.seek_to(t),
            Command::SeekBy(d) => {
                let t = self.position() + d;
                self.seek_to(t);
            }
            Command::SetVolume(v) => self.set_volume(v),
            Command::ToggleMute => self.set_muted(!self.muted),
        }
    }

    fn pause(&mut self) {
        // Freeze the reported position at the paused point.
        self.engine.set_paused(true);
        self.status = Status::Paused;
        self.notes.push(Note::StateChanged);
    }

    fn resume(&mut self) {
        self.engine.set_paused(false);
        self.status = Status::Playing;
        self.notes.push(Note::StateChanged);
    }

    /// Play: the current (stopped) entry, else the first playable entry of the viewed list.
    fn play_from_idle(&mut self) {
        if let Some(c) = self.current.clone()
            && self.lists.get(c.list).is_some_and(|l| l.playlist.get(c.entry).is_some())
        {
            self.play_entry(c.list, c.entry, 0.0);
            return;
        }
        let list = self.viewing;
        let first = self.nav.next(&self.lists[list].playlist, None, false, |e| e.status.is_playable());
        if let Some(id) = first {
            self.play_entry(list, id, 0.0);
        }
    }

    pub fn stop(&mut self) {
        self.engine.stop();
        self.status = Status::Stopped;
        self.pending_position = Some(0.0);
        self.preload = None;
        self.notes.push(Note::StateChanged);
    }

    fn skip(&mut self, forward: bool) {
        let list = self.playing_list();
        let current = self.current.as_ref().map(|c| c.entry);
        let playlist = &self.lists[list].playlist;
        let target = if forward {
            self.nav.next(playlist, current, false, |e| e.status.is_playable())
        } else {
            self.nav.previous(playlist, current, |e| e.status.is_playable())
        };
        match target {
            Some(id) => {
                // Paused stays paused on the new track; otherwise play.
                let play = match self.status {
                    Status::Paused => false,
                    Status::Loading => !self.load_paused,
                    Status::Playing | Status::Stopped => true,
                };
                self.failures = 0;
                self.start(list, id, 0.0, play);
            }
            None if forward => self.stop(),
            None => self.seek_to(0.0),
        }
    }

    pub fn seek_to(&mut self, secs: f64) {
        let Some(c) = &self.current else { return };
        let max = self.duration().unwrap_or(f64::MAX);
        let t = secs.clamp(0.0, max);
        if self.status == Status::Stopped {
            // Seeking while stopped starts paused at that position.
            let (list, entry) = (c.list, c.entry);
            self.start(list, entry, t, false);
            return;
        }
        self.pending_position = Some(t);
        self.engine.seek(t);
        self.notes.push(Note::StateChanged);
    }

    /// Process one engine event.
    pub fn on_engine_event(&mut self, ev: EngineEvent) {
        let current_token = self.current.as_ref().map(|c| c.token);
        match ev {
            EngineEvent::Loaded { token, info } if Some(token) == current_token => {
                let c = self.current.as_mut().unwrap();
                c.info = Some(info.clone());
                let (list, entry) = (c.list, c.entry);
                if let Some(e) = self.lists[list].playlist.get_mut(entry) {
                    e.status = EntryStatus::Ok;
                    if e.info.duration.is_none() {
                        e.info.duration = info.duration;
                    }
                }
                self.failures = 0;
                self.pending_position = None;
                self.status = if self.load_paused { Status::Paused } else { Status::Playing };
                self.refresh_preload();
                self.notes.push(Note::StateChanged);
            }
            EngineEvent::LoadFailed { token, error } if Some(token) == current_token => {
                let c = self.current.clone().unwrap();
                if let Some(e) = self.lists[c.list].playlist.get_mut(c.entry) {
                    e.status = match &error {
                        LoadError::NotFound => EntryStatus::Missing,
                        LoadError::Device(_) => EntryStatus::Unknown,
                        other => EntryStatus::Unsupported(other.to_string()),
                    };
                }
                let name = c.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.notes.push(Note::Error(format!("Cannot play {name}: {error}")));
                self.pending_position = Some(0.0);
                if matches!(error, LoadError::Device(_)) {
                    self.status = Status::Stopped;
                    self.notes.push(Note::StateChanged);
                    return;
                }
                // Move on, but never loop forever over a list of broken files.
                self.failures += 1;
                if self.failures < self.lists[c.list].playlist.len().max(1) {
                    self.advance_after(c.list, c.entry, true);
                } else {
                    self.failures = 0;
                    self.status = Status::Stopped;
                    self.notes.push(Note::StateChanged);
                }
            }
            EngineEvent::PreloadFailed { token, error } => {
                if let Some((t, list, id)) = self.preload
                    && t == token
                {
                    if let Some(e) = self.lists[list].playlist.get_mut(id) {
                        e.status = match error {
                            LoadError::NotFound => EntryStatus::Missing,
                            other => EntryStatus::Unsupported(other.to_string()),
                        };
                    }
                    self.preload = None;
                    self.refresh_preload();
                }
            }
            EngineEvent::TrackStarted { token, info } => {
                // Gapless transition into the preloaded entry.
                match self.preload {
                    Some((t, list, id)) if t == token => {
                        self.preload = None;
                        let path = self.lists[list].playlist.get(id).map(|e| e.path.clone()).unwrap_or_default();
                        self.current = Some(Current { list, entry: id, path, token, info: Some(info.clone()) });
                        if let Some(e) = self.lists[list].playlist.get_mut(id) {
                            e.status = EntryStatus::Ok;
                            if e.info.duration.is_none() {
                                e.info.duration = info.duration;
                            }
                        }
                        self.pending_position = None;
                        self.nav.sync(&self.lists[list].playlist, Some(id));
                        self.refresh_preload();
                        self.notes.push(Note::TrackChanged);
                        self.notes.push(Note::StateChanged);
                    }
                    _ => {
                        // The engine moved into a track that is no longer wanted.
                        // Restart the right one so state and audio agree.
                        if let Some(c) = self.current.clone() {
                            self.advance_after(c.list, c.entry, true);
                        }
                    }
                }
            }
            EngineEvent::Finished { token } if Some(token) == current_token => {
                let c = self.current.clone().unwrap();
                self.advance_after(c.list, c.entry, true);
            }
            EngineEvent::Seeked { token, .. } if Some(token) == current_token => {
                self.pending_position = None;
            }
            EngineEvent::PlaybackError { token, message } if Some(token) == current_token => {
                self.notes.push(Note::Error(format!("Playback problem: {message}")));
            }
            EngineEvent::DeviceLost { message } => {
                self.notes.push(Note::Error(format!("Audio output unavailable: {message}. Retrying…")));
            }
            EngineEvent::DeviceReady { name } => {
                self.notes.push(Note::Info(format!("Audio output: {name}")));
            }
            _ => {} // Stale event for a token we no longer care about.
        }
    }

    /// Move to the entry after `entry` automatically (track end or load failure).
    fn advance_after(&mut self, list: usize, entry: EntryId, auto: bool) {
        let playlist = &self.lists[list].playlist;
        let next = self.nav.next(playlist, Some(entry), auto, |e| e.status.is_playable());
        match next {
            Some(id) => {
                self.start(list, id, 0.0, true);
            }
            None => {
                self.status = Status::Stopped;
                self.pending_position = Some(0.0);
                self.preload = None;
                self.notes.push(Note::StateChanged);
            }
        }
    }

    /// Tell the engine which entry follows the current one, for gapless playback.
    /// Called after anything that could change the answer.
    pub fn refresh_preload(&mut self) {
        let Some(c) = self.current.clone() else {
            self.engine.preload(None);
            return;
        };
        if !matches!(self.status, Status::Playing | Status::Paused) {
            return;
        }
        let playlist = &self.lists[c.list].playlist;
        self.nav.sync(playlist, Some(c.entry));
        let next = self.nav.clone().next(playlist, Some(c.entry), true, |e| e.status.is_playable());
        let wanted = next.and_then(|id| playlist.get(id).map(|e| (id, e.path.clone())));
        match (wanted, self.preload) {
            (Some((id, _)), Some((_, l, pid))) if l == c.list && pid == id => {}
            (Some((id, path)), _) => {
                let token = self.token();
                self.preload = Some((token, c.list, id));
                self.engine.preload(Some((token, path)));
            }
            (None, _) => {
                self.preload = None;
                self.engine.preload(None);
            }
        }
    }

    // ----- Playlist editing (keeps navigation and preload consistent) -----

    pub fn edit_list(&mut self, list: usize, f: impl FnOnce(&mut Playlist)) {
        f(&mut self.lists[list].playlist);
        let current = self.current.as_ref().filter(|c| c.list == list).map(|c| c.entry);
        self.nav.sync(&self.lists[list].playlist, current);
        if self.playing_list() == list {
            self.refresh_preload();
        }
        self.notes.push(Note::PlaylistsChanged);
    }

    pub fn remove_entries(&mut self, list: usize, ids: &HashSet<EntryId>) -> usize {
        let mut n = 0;
        self.edit_list(list, |p| n = p.remove(ids));
        n
    }

    /// Append copies of entries of `from` to the end of `to`, in the order they have in `from`.
    /// The copies get their own ids, so either list can be edited or played on its own.
    pub fn copy_entries(&mut self, from: usize, ids: &HashSet<EntryId>, to: usize) -> usize {
        if from == to || from >= self.lists.len() || to >= self.lists.len() {
            return 0;
        }
        let copies: Vec<PlaylistEntry> = self.lists[from]
            .playlist
            .entries()
            .iter()
            .filter(|e| ids.contains(&e.id))
            .map(|e| PlaylistEntry { id: EntryId::new(), ..e.clone() })
            .collect();
        let n = copies.len();
        self.edit_list(to, |p| {
            p.insert(p.len(), copies);
        });
        n
    }

    pub fn add_list(&mut self, doc: PlaylistDoc) -> usize {
        self.lists.push(doc);
        self.notes.push(Note::PlaylistsChanged);
        self.lists.len() - 1
    }

    /// Remove a playlist. The playing list cannot be removed while playing from it.
    pub fn remove_list(&mut self, index: usize) -> Option<PlaylistDoc> {
        if self.lists.len() <= 1 || self.current.as_ref().is_some_and(|c| c.list == index && self.status != Status::Stopped) {
            return None;
        }
        let doc = self.lists.remove(index);
        if let Some(c) = self.current.as_mut() {
            if c.list == index {
                self.current = None;
            } else if c.list > index {
                c.list -= 1;
            }
        }
        if self.viewing >= self.lists.len() || self.viewing > index {
            self.viewing = self.viewing.saturating_sub(1).min(self.lists.len() - 1);
        }
        self.notes.push(Note::PlaylistsChanged);
        Some(doc)
    }

    pub fn drain_notes(&mut self) -> Vec<Note> {
        std::mem::take(&mut self.notes)
    }
}

#[cfg(test)]
#[path = "player_tests.rs"]
mod tests;
