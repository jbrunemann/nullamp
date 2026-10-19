//! The single-window user interface and the glue between UI, controller and workers.

pub mod deck;
pub mod dialogs;
mod inspector;
mod menu;
mod playlist_view;
mod setlist_view;
pub mod theme;
mod visualizer;
pub(crate) mod widgets;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use egui::{Key, KeyboardShortcut, Modifiers, TextureHandle, vec2};
use nullamp_audio::{AudioEngine, EngineEvent, OutputDevice, OutputKind};
use nullamp_core::command::{Command, CommandSource};
use nullamp_core::{EntryId, EntryStatus, PlaylistEntry};
use nullamp_meta::FileMetadata;
use nullamp_sysmedia::{NowPlaying, PlaybackState, SystemEvent, SystemMedia};

use crate::player::{Note, Player, Status};
use crate::store::{PlaylistDoc, Settings, Store};
use crate::workers::{Job, JobResult, Workers};

/// How often the resume position is written while a track plays.
const RESUME_SAVE_INTERVAL: Duration = Duration::from_secs(30);
const STATUS_HEIGHT: f32 = 24.0;
const DEFAULT_HEIGHT: f32 = 720.0;
/// Keep in step with the minimum window size in `main.rs`.
const MIN_WIDTH: f32 = 640.0;
const MIN_FULL_HEIGHT: f32 = 420.0;


#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SideTab {
    Setlist,
    Inspector,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageKind {
    Info,
    Warn,
    Error,
}

pub struct Message {
    pub text: String,
    pub kind: MessageKind,
    pub at: Instant,
}

/// Selection in the playlist editor (by id, so it survives reordering).
#[derive(Default)]
pub struct Selection {
    pub ids: HashSet<EntryId>,
    pub anchor: Option<EntryId>,
    pub cursor: Option<EntryId>,
}

/// What the deck shows about the current file, loaded in the background.
#[derive(Default)]
pub struct NowState {
    pub meta: Option<FileMetadata>,
    pub art: Option<TextureHandle>,
    pub retro_art: Option<TextureHandle>,
    /// The deck shows the original cover instead of the retro one until the next click or track.
    pub show_original_art: bool,
    pub art_file: Option<PathBuf>,
    /// Loudness overview behind the timeline; arrives after the track starts.
    pub waveform: Option<nullamp_audio::Waveform>,
    /// Tempo found by analysing the audio; arrives after the track starts, and not at all
    /// for files whose tags state a BPM.
    pub tempo: Option<nullamp_audio::Tempo>,
    /// While a mix plays: the span (start, end in seconds) of its active track that a tempo was
    /// last asked for, and the tempo found for it once that arrives.
    pub segment_requested: Option<(f64, f64)>,
    pub segment_tempo: Option<nullamp_audio::Tempo>,
}

/// The audio output as the engine last reported it.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputState {
    pub name: String,
    pub id: String,
    pub kind: OutputKind,
    pub sample_rate: u32,
    pub channels: u16,
    /// The chosen device is unavailable, so the system default plays instead.
    pub fell_back: bool,
}

pub struct App {
    pub player: Player<AudioEngine>,
    engine_events: Receiver<EngineEvent>,
    system_events: Receiver<SystemEvent>,
    sysmedia: Option<SystemMedia>,
    pub workers: Workers,
    pub store: Store,
    pub settings: Settings,
    pub selection: Selection,
    pub pl_view: playlist_view::ViewState,
    pub scroll_to: Option<EntryId>,
    pub now: NowState,
    pub mix: setlist_view::MixState,
    pub inspector: inspector::InspectorState,
    pub side_open: bool,
    pub side_tab: SideTab,
    pub playlist_open: bool,
    /// Whether the window is currently shrunk to just the player (`None` before the
    /// first frame) and the height to return to when it grows back.
    compact: Option<bool>,
    full_height: f32,
    pub message: Option<Message>,
    pub dialogs: dialogs::Dialogs,
    pub show_remaining: bool,
    /// `None` while no output device is open.
    pub output: Option<OutputState>,
    /// Outputs offered by the system, refreshed whenever the picker opens.
    pub output_devices: Vec<OutputDevice>,
    pub viz: visualizer::VizState,
    playlists_dirty_at: Option<Instant>,
    dirty_lists: HashSet<String>,
    settings_dirty_at: Option<Instant>,
    /// When the settings were last written, to space out saves of the resume position.
    settings_saved_at: Instant,
    /// Pending folder/file additions: request -> (list key, insert index).
    pub pending_adds: std::collections::HashMap<u64, (String, Option<usize>)>,
    last_sys_metadata: Option<NowPlaying>,
    #[cfg(target_os = "macos")]
    native_menu: Option<menu::NativeMenu>,
    ctx: egui::Context,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, mut store: Store) -> Self {
        let ctx = cc.egui_ctx.clone();
        let waker = {
            let ctx = ctx.clone();
            Arc::new(move || ctx.request_repaint())
        };

        let settings = store.load_settings();
        theme::apply(&ctx, theme::Appearance::from_setting(&settings.appearance));
        let lists = store.load_playlists(&settings.playlist_order);
        let (engine, engine_events) = AudioEngine::start_with_device(waker.clone(), settings.output_device.clone());
        let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(7);
        let mut player = Player::new(engine, lists, seed);
        player.set_volume(settings.volume);
        player.set_muted(settings.muted);
        player.nav.repeat = settings.repeat;
        if let Some(active) = &settings.active_playlist
            && let Some(i) = player.lists.iter().position(|d| &d.key == active)
        {
            player.viewing = i;
        }
        if settings.shuffle {
            player.set_shuffle(true);
        }

        let workers = Workers::start(store.paths.clone(), waker);
        let (system_tx, system_events) = crossbeam_channel::unbounded::<SystemEvent>();
        let sysmedia = {
            let tx = system_tx.clone();
            let ctx = ctx.clone();
            SystemMedia::new(
                move |ev| {
                    let _ = tx.send(ev);
                    ctx.request_repaint();
                },
                window_handle(cc),
            )
        };

        let side_tab = if settings.side_panel_tab == "inspector" { SideTab::Inspector } else { SideTab::Setlist };
        let mut app = App {
            player,
            engine_events,
            system_events,
            sysmedia: Some(sysmedia),
            workers,
            side_open: settings.side_panel_open,
            side_tab,
            playlist_open: settings.playlist_open,
            compact: None,
            full_height: DEFAULT_HEIGHT,
            settings,
            store,
            selection: Selection::default(),
            pl_view: Default::default(),
            scroll_to: None,
            now: NowState::default(),
            mix: Default::default(),
            inspector: Default::default(),
            message: None,
            dialogs: dialogs::Dialogs::new(),
            show_remaining: false,
            output: None,
            output_devices: Vec::new(),
            viz: Default::default(),
            playlists_dirty_at: None,
            dirty_lists: HashSet::new(),
            settings_dirty_at: None,
            settings_saved_at: Instant::now(),
            pending_adds: Default::default(),
            last_sys_metadata: None,
            #[cfg(target_os = "macos")]
            native_menu: None,
            ctx,
        };
        for w in std::mem::take(&mut app.store.load_warnings) {
            app.warn(w);
        }
        app.scan_all_lists();
        app.restore_session();
        if let Some(s) = app.sysmedia.as_mut() {
            s.clear_metadata();
            s.set_playback(PlaybackState::Stopped);
        }
        app
    }

    /// Put the app's own menu bar in place of the default one. Main thread only, so it is
    /// not part of `new` (tests build the app on other threads).
    pub fn install_native_menu(&mut self) {
        #[cfg(target_os = "macos")]
        {
            self.native_menu = menu::NativeMenu::install(self.ctx.clone());
        }
    }

    fn on_menu_action(&mut self, action: menu::MenuAction) {
        match action {
            menu::MenuAction::About => {
                self.dialogs.settings = Some(dialogs::SettingsTab::About);
                self.ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
            menu::MenuAction::OpenConfig => self.open_config_file(),
        }
    }

    /// Open `settings.json` in the default text editor, writing the current settings first
    /// if the file does not exist yet.
    fn open_config_file(&mut self) {
        let path = self.store.paths.config.join("settings.json");
        if !path.exists() {
            self.sync_settings_from_state();
            if let Err(e) = self.store.save_settings(&self.settings) {
                self.error(format!("Cannot create {}: {e}", path.display()));
                return;
            }
        }
        if let Err(e) = std::process::Command::new("open").arg("-t").arg(&path).spawn() {
            self.error(format!("Cannot open {}: {e}", path.display()));
        }
    }

    // ----- Messages -----

    pub fn info(&mut self, text: impl Into<String>) {
        self.message = Some(Message { text: text.into(), kind: MessageKind::Info, at: Instant::now() });
    }

    pub fn warn(&mut self, text: impl Into<String>) {
        self.message = Some(Message { text: text.into(), kind: MessageKind::Warn, at: Instant::now() });
    }

    pub fn error(&mut self, text: impl Into<String>) {
        let text = text.into();
        log::warn!("{text}");
        self.message = Some(Message { text, kind: MessageKind::Error, at: Instant::now() });
    }

    // ----- Persistence -----

    pub fn mark_list_dirty(&mut self, list: usize) {
        if let Some(doc) = self.player.lists.get(list) {
            self.dirty_lists.insert(doc.key.clone());
        }
        self.playlists_dirty_at.get_or_insert_with(Instant::now);
    }

    pub fn mark_settings_dirty(&mut self) {
        self.settings_dirty_at.get_or_insert_with(Instant::now);
    }

    fn flush_saves(&mut self, force: bool) {
        // The resume position moves while playing, but a crash only costs a little of it,
        // so write it rarely instead of every second.
        if self.player.status == Status::Playing && self.settings_saved_at.elapsed() > RESUME_SAVE_INTERVAL {
            self.mark_settings_dirty();
        }
        let due = |t: Option<Instant>| t.is_some_and(|t| force || t.elapsed() > Duration::from_millis(800));
        if due(self.playlists_dirty_at) {
            self.playlists_dirty_at = None;
            for key in std::mem::take(&mut self.dirty_lists) {
                if let Some(doc) = self.player.lists.iter().find(|d| d.key == key)
                    && let Err(e) = self.store.save_playlist(doc)
                {
                    self.error(format!("Cannot save playlist {}: {e}", doc.playlist.name));
                }
            }
            self.sync_settings_from_state();
        }
        if due(self.settings_dirty_at) {
            self.settings_dirty_at = None;
            self.settings_saved_at = Instant::now();
            self.sync_settings_from_state();
            if let Err(e) = self.store.save_settings(&self.settings) {
                self.error(format!("Cannot save settings: {e}"));
            }
        }
        setlist_view::flush_setlist_save(self, force);
    }

    fn sync_settings_from_state(&mut self) {
        let s = &mut self.settings;
        s.volume = self.player.volume;
        s.muted = self.player.muted;
        s.shuffle = self.player.nav.shuffle();
        s.repeat = self.player.nav.repeat;
        s.playlist_order = self.player.lists.iter().map(|d| d.key.clone()).collect();
        s.active_playlist = self.player.lists.get(self.player.viewing).map(|d| d.key.clone());
        s.side_panel_open = self.side_open;
        s.playlist_open = self.playlist_open;
        s.side_panel_tab = match self.side_tab {
            SideTab::Setlist => "setlist".into(),
            SideTab::Inspector => "inspector".into(),
        };
        s.resume = self.player.current.as_ref().and_then(|c| {
            let doc = self.player.lists.get(c.list)?;
            Some(crate::store::ResumePoint {
                playlist: doc.key.clone(),
                index: doc.playlist.index_of(c.entry)?,
                path: c.path.clone(),
                position: self.player.position(),
            })
        });
    }

    fn restore_session(&mut self) {
        let Some(r) = self.settings.resume.clone() else { return };
        let Some(list) = self.player.lists.iter().position(|d| d.key == r.playlist) else { return };
        let entry = self.player.lists[list].playlist.entries().get(r.index).filter(|e| e.path == r.path).map(|e| e.id);
        let Some(entry) = entry else { return };
        self.scroll_to = Some(entry);
        self.selection.ids = HashSet::from([entry]);
        self.selection.cursor = Some(entry);
        if self.settings.resume_playback {
            self.player.play_entry(list, entry, r.position);
        }
    }

    // ----- Background scanning -----

    fn scan_all_lists(&mut self) {
        let mut unknown = Vec::new();
        let mut known = Vec::new();
        for doc in &self.player.lists {
            for e in doc.playlist.entries() {
                if e.info == Default::default() { unknown.push(e.path.clone()) } else { known.push(e.path.clone()) }
            }
        }
        unknown.sort();
        unknown.dedup();
        known.sort();
        known.dedup();
        self.workers.scan(unknown);
        self.workers.check_exists(known);
    }

    /// Add files (and expanded folders) to a list at `insert_at` (None = end).
    pub fn add_paths(&mut self, list: usize, paths: Vec<PathBuf>, insert_at: Option<usize>) {
        if paths.is_empty() {
            return;
        }
        let playlists: Vec<PathBuf> = paths.iter().filter(|p| crate::workers::is_playlist_file(p)).cloned().collect();
        for p in playlists {
            let request = self.workers.request_id();
            self.pending_adds.insert(request, (self.player.lists[list].key.clone(), insert_at));
            self.workers.submit(Job::ImportPlaylist { request, path: p });
        }
        let rest: Vec<PathBuf> = paths.into_iter().filter(|p| !crate::workers::is_playlist_file(p)).collect();
        if rest.is_empty() {
            return;
        }
        // Plain audio files are inserted right away; folders are expanded in the background.
        if rest.iter().all(|p| p.is_file()) {
            self.insert_entries(list, rest.into_iter().filter(|p| crate::workers::is_audio_file(p)).map(PlaylistEntry::new).collect(), insert_at);
        } else {
            let request = self.workers.request_id();
            self.pending_adds.insert(request, (self.player.lists[list].key.clone(), insert_at));
            self.workers.submit(Job::ExpandPaths { request, paths: rest });
            self.info("Scanning folders…");
        }
    }

    pub fn insert_entries(&mut self, list: usize, entries: Vec<PlaylistEntry>, insert_at: Option<usize>) {
        if entries.is_empty() {
            return;
        }
        let to_scan: Vec<PathBuf> = entries.iter().filter(|e| e.info == Default::default()).map(|e| e.path.clone()).collect();
        let n = entries.len();
        let mut ids = Vec::new();
        self.player.edit_list(list, |p| {
            let at = insert_at.unwrap_or(p.len());
            ids = p.insert(at, entries);
        });
        if list == self.player.viewing {
            self.selection.ids = ids.iter().copied().collect();
            self.selection.anchor = ids.first().copied();
            self.selection.cursor = ids.last().copied();
            self.scroll_to = ids.first().copied();
        }
        self.workers.scan(to_scan);
        self.info(format!("Added {n} track{}", if n == 1 { "" } else { "s" }));
    }

    fn apply_info(&mut self, path: PathBuf, info: Result<nullamp_core::TrackInfo, String>) {
        let mut touched = Vec::new();
        for (i, doc) in self.player.lists.iter_mut().enumerate() {
            for e in doc.playlist.entries_with_path_mut(&path) {
                match &info {
                    Ok(info) => {
                        let detected = e.info.bpm.filter(|_| e.info.bpm_detected);
                        e.info = info.clone();
                        // A tempo found by listening stays until the tags state one.
                        if e.info.bpm.is_none() && !e.info.bpm_off && detected.is_some() {
                            (e.info.bpm, e.info.bpm_detected) = (detected, true);
                        }
                        if e.status != EntryStatus::Ok {
                            e.status = EntryStatus::Unknown;
                        }
                    }
                    Err(_) if !path.exists() => e.status = EntryStatus::Missing,
                    Err(msg) => e.status = EntryStatus::Unsupported(msg.clone()),
                }
                touched.push(i);
            }
        }
        touched.dedup();
        for i in touched {
            self.mark_list_dirty(i);
        }
    }

    // ----- Event processing (runs every frame, also while hidden) -----

    fn process_events(&mut self) {
        while let Ok(ev) = self.engine_events.try_recv() {
            self.on_output_event(&ev);
            self.player.on_engine_event(ev);
        }
        while let Ok(ev) = self.system_events.try_recv() {
            match ev {
                SystemEvent::Command(cmd) => self.player.command(cmd, CommandSource::System),
                SystemEvent::Raise => {
                    self.ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    self.ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    self.ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                SystemEvent::Quit => self.ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            }
        }
        #[cfg(target_os = "macos")]
        while let Some(action) = self.native_menu.as_ref().and_then(|m| m.poll()) {
            self.on_menu_action(action);
        }
        while let Ok(r) = self.workers.results.try_recv() {
            self.on_job_result(r);
        }
        while let Ok(r) = self.dialogs.results.try_recv() {
            dialogs::on_dialog_result(self, r);
        }
        for note in self.player.drain_notes() {
            match note {
                Note::TrackChanged => self.on_track_changed(),
                Note::StateChanged => self.publish_playback_state(),
                Note::PlaylistsChanged => {
                    let viewing = self.player.viewing;
                    self.mark_list_dirty(viewing);
                    if let Some(c) = &self.player.current {
                        let l = c.list;
                        self.mark_list_dirty(l);
                    }
                }
                Note::Error(e) => self.error(e),
                Note::Info(i) => self.info(i),
            }
        }
        setlist_view::update_active(self);
        self.update_segment_tempo();
        self.publish_now_playing();
        self.flush_saves(false);
    }

    fn on_output_event(&mut self, ev: &EngineEvent) {
        match ev {
            EngineEvent::OutputOpened { name, id, kind, sample_rate, channels, fell_back } => {
                // Have the picker's list ready before the first click, so it opens at its full size.
                self.player.engine.request_output_devices();
                if *fell_back && self.output.as_ref().is_none_or(|o| !o.fell_back) {
                    self.warn(format!("The chosen audio output is not available; using {name}"));
                }
                self.output = Some(OutputState { name: name.clone(), id: id.clone(), kind: *kind, sample_rate: *sample_rate, channels: *channels, fell_back: *fell_back });
            }
            EngineEvent::DeviceLost { .. } => self.output = None,
            EngineEvent::OutputDevices(devices) => self.output_devices = devices.clone(),
            _ => {}
        }
    }

    /// Play through `device` (`None` = system default) and remember the choice.
    pub fn choose_output(&mut self, device: Option<String>) {
        if self.settings.output_device == device {
            return;
        }
        self.settings.output_device = device.clone();
        self.mark_settings_dirty();
        self.player.engine.set_output_device(device);
    }

    fn on_job_result(&mut self, r: JobResult) {
        match r {
            JobResult::Info { path, info } => self.apply_info(path, info),
            JobResult::Cover { path, art } => self.pl_view.art.insert(&self.ctx, path, art),
            JobResult::Missing { paths } => {
                let set: HashSet<PathBuf> = paths.into_iter().collect();
                for doc in &mut self.player.lists {
                    let ids: Vec<EntryId> = doc.playlist.entries().iter().filter(|e| set.contains(&e.path)).map(|e| e.id).collect();
                    for id in ids {
                        if let Some(e) = doc.playlist.get_mut(id) {
                            e.status = EntryStatus::Missing;
                        }
                    }
                }
            }
            JobResult::Expanded { request, paths, skipped } => {
                let Some((key, at)) = self.pending_adds.remove(&request) else { return };
                let Some(list) = self.player.lists.iter().position(|d| d.key == key) else { return };
                if paths.is_empty() {
                    self.warn("No audio files found");
                    return;
                }
                self.insert_entries(list, paths.into_iter().map(PlaylistEntry::new).collect(), at);
                if skipped > 0 {
                    self.info(format!("Skipped {skipped} non-audio item{}", if skipped == 1 { "" } else { "s" }));
                }
            }
            JobResult::Imported { request, result } => {
                let Some((key, at)) = self.pending_adds.remove(&request) else { return };
                let Some(list) = self.player.lists.iter().position(|d| d.key == key) else { return };
                match result {
                    Ok((_, items, skipped)) => {
                        let entries: Vec<PlaylistEntry> = items.into_iter().map(|(p, i)| PlaylistEntry::with_info(p, i)).collect();
                        let paths: Vec<PathBuf> = entries.iter().map(|e| e.path.clone()).collect();
                        self.insert_entries(list, entries, at);
                        self.workers.check_exists(paths);
                        if skipped > 0 {
                            self.warn(format!("Imported playlist; {skipped} unsupported entr{} skipped (streams/URLs)", if skipped == 1 { "y" } else { "ies" }));
                        }
                    }
                    Err(e) => self.error(format!("Cannot import playlist: {e}")),
                }
            }
            JobResult::Exported { path, result } => match result {
                Ok(()) => self.info(format!("Exported {}", path.display())),
                Err(e) => self.error(format!("Export failed: {e}")),
            },
            JobResult::NowPlaying(info) => self.on_now_playing(*info),
            JobResult::Waveform { token, result } => {
                if self.player.current.as_ref().is_some_and(|c| c.token == token) {
                    match result {
                        Ok(wave) => self.now.waveform = Some(wave),
                        Err(e) => log::debug!("waveform: {e}"),
                    }
                }
            }
            JobResult::Tempo { token, span, result } => {
                if self.player.current.as_ref().is_some_and(|c| c.token == token) {
                    match (result, span) {
                        (Ok(tempo), None) => {
                            self.now.tempo = tempo;
                            if let (Some(t), Some(path)) = (tempo, self.player.current.as_ref().map(|c| c.path.clone())) {
                                self.note_detected_bpm(&path, t.bpm);
                            }
                        }
                        // The active track may have changed again while this was being worked out.
                        (Ok(tempo), Some(span)) if self.now.segment_requested == Some(span) => self.now.segment_tempo = tempo,
                        (Ok(_), Some(_)) => {}
                        (Err(e), _) => log::debug!("tempo: {e}"),
                    }
                }
            }
            JobResult::SetlistSaved(r) => {
                if let Err(e) = r {
                    self.error(format!("Cannot save setlist: {e}"));
                }
            }
            JobResult::Text { request, path, text } => setlist_view::on_text_loaded(self, request, path, text),
            JobResult::Rediscovered { request, setlist } => setlist_view::on_rediscovered(self, request, setlist),
            JobResult::TextWritten { path, result } => match result {
                Ok(()) => self.info(format!("Saved {}", path.display())),
                Err(e) => self.error(format!("Cannot write {}: {e}", path.display())),
            },
            JobResult::Inspected { .. }
            | JobResult::Image { .. }
            | JobResult::Written { .. }
            | JobResult::Restored { .. } => inspector::on_result(self, r),
        }
    }

    fn on_track_changed(&mut self) {
        let Some(c) = self.player.current.clone() else { return };
        self.mark_settings_dirty();
        self.now = NowState::default();
        setlist_view::reset_for_track(self, c.token, c.path.clone());
        self.workers.submit(Job::NowPlaying { token: c.token, path: c.path.clone() });
        self.workers.request_waveform(c.token, c.path.clone());
        self.workers.request_tempo(c.token, c.path.clone(), None);
        self.scroll_to = Some(c.entry);
        if self.settings.inspector_follows_playing {
            inspector::load_targets(self, vec![c.path.clone()]);
        }
        let title = self.player.current_entry().map(|e| e.info.display_line(&e.path)).unwrap_or_default();
        self.ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!("{title} — Nullamp")));
    }

    /// Show a tempo found by listening in the playlist rows of `path` (never over one from the tags).
    fn note_detected_bpm(&mut self, path: &Path, bpm: f32) {
        let mut touched = Vec::new();
        for (i, doc) in self.player.lists.iter_mut().enumerate() {
            for e in doc.playlist.entries_with_path_mut(path) {
                if !e.info.bpm_off && (e.info.bpm.is_none() || e.info.bpm_detected) {
                    (e.info.bpm, e.info.bpm_detected) = (Some(bpm), true);
                    touched.push(i);
                }
            }
        }
        touched.dedup();
        for i in touched {
            self.mark_list_dirty(i);
        }
    }

    /// While a mix plays, ask for the tempo of its active track whenever that changes: the
    /// tempo of a whole mix says little when it moves from track to track.
    fn update_segment_tempo(&mut self) {
        let span = setlist_view::shown(self).zip(self.mix.active).and_then(|(s, i)| s.span(i, self.player.duration()));
        if span == self.now.segment_requested {
            return;
        }
        let leaving = self.now.segment_requested.is_some() && span.is_none();
        self.now.segment_requested = span;
        self.now.segment_tempo = None;
        let Some(c) = &self.player.current else { return };
        match span {
            Some(span) => self.workers.request_tempo(c.token, c.path.clone(), Some(span)),
            // Asking for the track cancelled any whole-file analysis still running.
            None if leaving && self.now.tempo.is_none() => self.workers.request_tempo(c.token, c.path.clone(), None),
            None => {}
        }
    }

    /// Whether the playing file's tags switch the tempo off (BPM `-1`).
    pub fn current_bpm_off(&self) -> bool {
        self.now.meta.as_ref().and_then(|m| m.fields.get(&nullamp_meta::Field::Bpm)).is_some_and(|v| nullamp_meta::is_bpm_off(v))
    }

    /// BPM of the playing track (of the active track while a mix plays) and whether it was
    /// detected (else it is from the file's tags). `None` when it is switched off.
    pub fn current_bpm(&self) -> Option<(f32, bool)> {
        if self.current_bpm_off() {
            return None;
        }
        if self.now.segment_requested.is_some() {
            return self.now.segment_tempo.map(|t| (t.bpm, true));
        }
        let tagged = self.now.meta.as_ref().and_then(|m| m.fields.get(&nullamp_meta::Field::Bpm)).and_then(|v| v.trim().parse::<f32>().ok());
        match (tagged, self.now.tempo) {
            (Some(bpm), _) => Some((bpm, false)),
            (None, Some(t)) => Some((t.bpm, true)),
            (None, None) => None,
        }
    }

    pub fn key_notation(&self) -> nullamp_core::KeyNotation {
        if self.settings.camelot { nullamp_core::KeyNotation::Camelot } else { nullamp_core::KeyNotation::Musical }
    }

    fn on_now_playing(&mut self, info: crate::workers::NowPlayingInfo) {
        // Results for a track that is no longer current are dropped.
        let current_path = self.player.current.as_ref().map(|c| c.path.clone());
        if current_path.as_ref() != Some(&info.path) {
            return;
        }
        if let Some(t) = &info.art {
            let img = egui::ColorImage::from_rgba_unmultiplied(t.size, &t.rgba);
            self.now.art = Some(self.ctx.load_texture("now-art", img, egui::TextureOptions::LINEAR));
        }
        if let Some(t) = &info.retro_art {
            let img = egui::ColorImage::from_rgba_unmultiplied(t.size, &t.rgba);
            self.now.retro_art = Some(self.ctx.load_texture("now-art-retro", img, egui::TextureOptions::LINEAR));
        }
        self.now.art_file = info.art_file.clone();
        match &info.meta {
            Ok(m) => {
                // Keep playlist rows in sync with what the file says now.
                let ti = m.track_info();
                self.apply_info(info.path.clone(), Ok(ti));
                self.now.meta = Some(m.clone());
            }
            Err(e) => log::debug!("metadata for now playing: {e}"),
        }
        setlist_view::on_now_playing(self, &info);
        self.last_sys_metadata = None;
    }

    // ----- OS media integration -----

    fn publish_playback_state(&mut self) {
        let position = self.player.position();
        let state = match self.player.status {
            Status::Playing | Status::Loading => PlaybackState::Playing { position },
            Status::Paused => PlaybackState::Paused { position },
            Status::Stopped => PlaybackState::Stopped,
        };
        if let Some(s) = self.sysmedia.as_mut() {
            s.set_playback(state);
        }
    }

    /// The OS overlay shows the constituent track while a mix plays, with the mix
    /// as the album so the relationship stays visible.
    fn publish_now_playing(&mut self) {
        let Some(entry) = self.player.current_entry() else { return };
        let mut np = NowPlaying {
            title: entry.info.display_title(&entry.path),
            artist: entry.info.artist.clone(),
            album: entry.info.album.clone(),
            duration: self.player.duration(),
            cover: self.now.art_file.clone(),
        };
        if let Some((setlist, idx)) = setlist_view::shown(self).zip(self.mix.active) {
            let e = &setlist.entries[idx];
            let mix_name = setlist.mix.title.clone().unwrap_or_else(|| np.title.clone());
            let dj = setlist.mix.dj.clone().or(np.artist.clone());
            np.title = e.full_title();
            np.artist = Some(e.display_artist().to_owned());
            np.album = Some(match dj {
                Some(dj) => format!("{mix_name} — {dj}"),
                None => mix_name,
            });
        }
        if self.last_sys_metadata.as_ref() != Some(&np) {
            if let Some(s) = self.sysmedia.as_mut() {
                s.set_metadata(&np);
            }
            self.last_sys_metadata = Some(np);
            self.publish_playback_state();
        }
    }

    // ----- Keyboard shortcuts -----

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let typing = ctx.egui_wants_keyboard_input();
        let cmd = |key| KeyboardShortcut::new(Modifiers::COMMAND, key);
        let cmd_shift = |key| KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, key);
        let consume = |s: KeyboardShortcut| ctx.input_mut(|i| i.consume_shortcut(&s));

        if consume(cmd(Key::O)) {
            dialogs::open_files(self);
        }
        if consume(cmd_shift(Key::O)) {
            dialogs::open_folder(self);
        }
        if consume(cmd(Key::I)) {
            self.toggle_side(SideTab::Inspector);
        }
        if consume(cmd(Key::L)) {
            self.toggle_side(SideTab::Setlist);
        }
        if consume(cmd(Key::P)) {
            self.toggle_playlist();
        }
        if consume(cmd(Key::J)) {
            self.scroll_to = self.player.current.as_ref().map(|c| c.entry);
        }
        if consume(KeyboardShortcut::new(Modifiers::NONE, Key::F1)) {
            self.dialogs.settings = if self.dialogs.settings == Some(dialogs::SettingsTab::Shortcuts) { None } else { Some(dialogs::SettingsTab::Shortcuts) };
        }
        if typing {
            return;
        }
        let pressed = |k| ctx.input(|i| i.key_pressed(k) && i.modifiers.is_none());
        let shift = |k| ctx.input(|i| i.key_pressed(k) && i.modifiers.shift_only());
        let mut run = |c: Command| self.player.command(c, CommandSource::Keyboard);
        if pressed(Key::Space) {
            run(Command::PlayPause);
        }
        // The classic Z X C V B row.
        if pressed(Key::Z) {
            run(Command::Previous);
        }
        if pressed(Key::X) {
            run(Command::Play);
        }
        if pressed(Key::C) {
            run(Command::PlayPause);
        }
        if pressed(Key::V) {
            run(Command::Stop);
        }
        if pressed(Key::B) {
            run(Command::Next);
        }
        if pressed(Key::ArrowRight) {
            run(Command::SeekBy(5.0));
        }
        if pressed(Key::ArrowLeft) {
            run(Command::SeekBy(-5.0));
        }
        if pressed(Key::M) {
            run(Command::ToggleMute);
        }
        if pressed(Key::Plus) || pressed(Key::Equals) {
            let v = self.player.volume + 0.05;
            self.player.command(Command::SetVolume(v), CommandSource::Keyboard);
            self.mark_settings_dirty();
        }
        if pressed(Key::Minus) {
            let v = self.player.volume - 0.05;
            self.player.command(Command::SetVolume(v), CommandSource::Keyboard);
            self.mark_settings_dirty();
        }
        if pressed(Key::S) {
            let on = !self.player.nav.shuffle();
            self.player.set_shuffle(on);
            self.mark_settings_dirty();
        }
        if pressed(Key::R) {
            let m = self.player.nav.repeat.cycle();
            self.player.set_repeat(m);
            self.mark_settings_dirty();
        }
        // Mix navigation: jump between setlist entries.
        if shift(Key::ArrowRight) {
            setlist_view::jump_relative(self, 1);
        }
        if shift(Key::ArrowLeft) {
            setlist_view::jump_relative(self, -1);
        }
        if self.playlist_open {
            playlist_view::handle_keys(self, ctx);
        }
    }

    pub fn toggle_playlist(&mut self) {
        self.playlist_open = !self.playlist_open;
        self.mark_settings_dirty();
    }

    /// With neither the playlist nor the side panel showing, the window is just the
    /// deck and the status bar, so shrink it to that and restore it afterwards.
    fn fit_window(&mut self, ctx: &egui::Context, deck_height: f32) {
        let compact = !self.playlist_open && !self.side_open;
        let before = self.compact.replace(compact);
        if before == Some(compact) {
            return;
        }
        let size = ctx.content_rect().size();
        if compact {
            if before == Some(false) {
                self.full_height = size.y;
            }
            let height = (deck_height + STATUS_HEIGHT).ceil();
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(vec2(MIN_WIDTH, height)));
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(vec2(size.x.max(MIN_WIDTH), height)));
        } else {
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(vec2(MIN_WIDTH, MIN_FULL_HEIGHT)));
            if before.is_some() {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(vec2(size.x, self.full_height)));
            }
        }
    }

    /// The side panel tab on screen. Single tracks show the inspector without changing
    /// the saved choice, so the next mix opens on its setlist again.
    pub fn shown_tab(&self) -> SideTab {
        if self.side_tab == SideTab::Setlist && !self.mix.is_mix { SideTab::Inspector } else { self.side_tab }
    }

    pub fn toggle_side(&mut self, tab: SideTab) {
        if self.side_open && self.shown_tab() == tab {
            self.side_open = false;
        } else {
            if tab == SideTab::Setlist && !self.mix.is_mix && self.player.current.is_some() {
                setlist_view::pin(self);
            }
            self.side_open = true;
            self.side_tab = tab;
            if tab == SideTab::Inspector && self.inspector.targets.is_empty() {
                inspector::load_on_open(self);
            }
        }
        self.mark_settings_dirty();
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        if dropped.is_empty() {
            return;
        }
        // Drop position decides the insertion point when over the playlist.
        let at = playlist_view::drop_index(self, ctx);
        let list = self.player.viewing;
        self.add_paths(list, dropped, at);
    }

    fn side_panel(&mut self, ui: &mut egui::Ui) {
        let shown = self.shown_tab();
        ui.horizontal(|ui| {
            if self.mix.is_mix && widgets::tab(ui, shown == SideTab::Setlist, "Setlist").clicked() {
                self.side_tab = SideTab::Setlist;
                self.mark_settings_dirty();
            }
            if widgets::tab(ui, shown == SideTab::Inspector, "Inspector").clicked() {
                self.side_tab = SideTab::Inspector;
                if self.inspector.targets.is_empty() {
                    inspector::load_on_open(self);
                }
                self.mark_settings_dirty();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("✕").on_hover_text("Hide panel").clicked() {
                    self.side_open = false;
                    self.mark_settings_dirty();
                }
            });
        });
        ui.add_space(4.0);
        match shown {
            SideTab::Setlist => setlist_view::show(self, ui),
            SideTab::Inspector => inspector::show(self, ui),
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_centered(|ui| {
            let pl = self.player.viewed();
            let (total, incomplete) = pl.total_duration();
            let missing = pl.entries().iter().filter(|e| !e.status.is_playable()).count();
            ui.label(
                egui::RichText::new(format!(
                    "{} tracks · {}{}",
                    pl.len(),
                    nullamp_core::time::format_clock(total),
                    if incomplete { "+" } else { "" }
                ))
                .color(theme::text_dim())
                .size(11.5),
            );
            if !self.selection.ids.is_empty() {
                ui.label(egui::RichText::new(format!("· {} selected", self.selection.ids.len())).color(theme::text_dim()).size(11.5));
            }
            if missing > 0 {
                ui.label(egui::RichText::new(format!("· {missing} unavailable")).color(theme::warn()).size(11.5));
            }
            if let Some(m) = &self.message {
                let ttl = if m.kind == MessageKind::Error { 12.0 } else { 6.0 };
                if m.at.elapsed().as_secs_f32() < ttl {
                    let color = match m.kind {
                        MessageKind::Info => theme::text_dim(),
                        MessageKind::Warn => theme::warn(),
                        MessageKind::Error => theme::err(),
                    };
                    ui.separator();
                    ui.add(egui::Label::new(egui::RichText::new(&m.text).color(color).size(11.5)).truncate());
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.sysmedia.as_ref().is_some_and(|s| !s.is_available()) {
                    ui.label(egui::RichText::new("media keys unavailable").size(11.0).color(theme::warn()))
                        .on_hover_text("The system media service could not be reached; in-app shortcuts still work.");
                }
            });
        });
    }

    fn schedule_repaint(&self, ctx: &egui::Context) {
        let playing = self.player.status == Status::Playing || self.player.status == Status::Loading;
        // Smooth progress and marquee while playing; idle otherwise. Hidden windows
        // still run `logic` on these wake-ups so track changes reach the OS overlay.
        let interval = if playing { Duration::from_millis(33) } else { Duration::from_millis(500) };
        ctx.request_repaint_after(interval);
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_events();
        self.schedule_repaint(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        theme::sync(&ctx);
        self.handle_shortcuts(&ctx);
        self.handle_dropped_files(&ctx);

        let deck = egui::Panel::top("deck")
            .frame(egui::Frame::new().fill(theme::deck_bg()).inner_margin(egui::Margin::symmetric(10, 8)))
            .resizable(false)
            .show(ui, |ui| theme::with_dark(ui, |ui| deck::show(self, ui)));
        self.fit_window(&ctx, deck.response.rect.height());
        egui::Panel::bottom("status")
            .frame(egui::Frame::new().fill(theme::bg()).inner_margin(egui::Margin::symmetric(10, 3)))
            .exact_size(STATUS_HEIGHT)
            .resizable(false)
            .show(ui, |ui| self.status_bar(ui));
        if self.side_open && self.playlist_open {
            egui::Panel::right("side")
                .frame(egui::Frame::new().fill(theme::panel()).inner_margin(egui::Margin::same(8)))
                .default_size(380.0)
                .size_range(300.0..=720.0)
                .resizable(true)
                .show(ui, |ui| self.side_panel(ui));
        }
        // Without the playlist, the side panel takes over the space below the deck.
        let central = egui::CentralPanel::default();
        if !self.playlist_open {
            let margin = if self.side_open { egui::Margin::same(8) } else { egui::Margin::same(0) };
            central.frame(egui::Frame::new().fill(if self.side_open { theme::panel() } else { theme::bg() }).inner_margin(margin)).show(ui, |ui| {
                if self.side_open {
                    self.side_panel(ui);
                }
            });
        } else {
            central
                .frame(egui::Frame::new().fill(theme::panel()).inner_margin(egui::Margin::same(0)))
                .show(ui, |ui| playlist_view::show(self, ui));
        }

        dialogs::show_modals(self, &ctx);
        dialogs::show_deck_overlay(self, &ctx, deck.response.rect);
    }

    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        self.flush_saves(true);
    }

    fn on_exit(&mut self) {
        self.sync_settings_from_state();
        self.flush_saves(true);
    }
}

#[cfg(target_os = "windows")]
fn window_handle(cc: &eframe::CreationContext<'_>) -> Option<*mut std::ffi::c_void> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match cc.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get() as *mut std::ffi::c_void),
        _ => None,
    }
}

#[cfg(not(target_os = "windows"))]
fn window_handle(_cc: &eframe::CreationContext<'_>) -> Option<*mut std::ffi::c_void> {
    None
}

/// Open the editor for the setlist entry at `index` (tests drive the UI through this).
#[cfg(test)]
pub fn select_setlist_entry(app: &mut App, index: usize) {
    let id = app.mix.setlist.as_ref().and_then(|s| s.entries.get(index)).map(|e| e.id);
    setlist_view::select(app, id);
}

#[cfg(test)]
pub fn inspect_selection(app: &mut App) {
    inspector::load_from_selection(app);
}

/// Shared by views: playlist doc lookup by key.
pub fn list_index(app: &App, key: &str) -> Option<usize> {
    app.player.lists.iter().position(|d: &PlaylistDoc| d.key == key)
}

/// "128", or "127.5" for a fractional tempo.
pub fn format_bpm(bpm: f32) -> String {
    if (bpm - bpm.round()).abs() < 0.05 { format!("{}", bpm.round()) } else { format!("{bpm:.1}") }
}
