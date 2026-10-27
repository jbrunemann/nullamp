//! The single-window user interface and the glue between UI, controller and workers.

#[cfg(target_os = "android")]
mod android_dialogs;
pub mod deck;
pub mod dialogs;
pub mod file_browser;
mod inspector;
mod menu;
mod playlist_view;
pub mod sections;
mod setlist_view;
pub mod spotify;
pub mod telegram;
pub mod theme;
mod visualizer;
pub(crate) mod widgets;

use std::collections::{HashMap, HashSet};
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
use sections::{Plan, Section, Wanted, Widths};
use crate::store::{ArtworkMode, PlaylistDoc, Settings, Store};
use crate::workers::{Job, JobResult, Workers};

/// How often the resume position is written while a track plays.
const RESUME_SAVE_INTERVAL: Duration = Duration::from_secs(30);
const STATUS_HEIGHT: f32 = 24.0;
/// The width "media keys unavailable" needs at the right end of the status bar.
const MEDIA_KEYS_NOTE_WIDTH: f32 = 150.0;
const DEFAULT_HEIGHT: f32 = 720.0;
/// Keep in step with the minimum window size in `main.rs`.
const MIN_WIDTH: f32 = 360.0;
const MIN_FULL_HEIGHT: f32 = 420.0;
/// The narrowest a setlist or inspector column can be dragged.
const SIDE_MIN_WIDTH: f32 = 300.0;

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
    pub art_file: Option<PathBuf>,
    /// Loudness overview behind the timeline; arrives after the track starts.
    pub waveform: Option<nullamp_audio::Waveform>,
    /// rekordbox's colour for each bucket of `waveform`, when the waveform came from there.
    pub wave_colors: Option<Vec<[u8; 3]>>,
    /// When `waveform` arrived, while it is still being revealed from left to right.
    pub wave_arrived: Option<Instant>,
    /// Tempo found by analysing the audio; arrives after the track starts, and not at all
    /// for files whose tags state a BPM.
    pub tempo: Option<nullamp_audio::Tempo>,
    /// While a mix plays: the span (start, end in seconds) of its active track that a tempo was
    /// last asked for, and the tempo found for it once that arrives.
    pub segment_requested: Option<(f64, f64)>,
    pub segment_tempo: Option<nullamp_audio::Tempo>,
    /// Covers of the tracks of the playing mix, found online or generated, by `entry_art::key`,
    /// asked for when a track first becomes active.
    pub entry_art: HashMap<String, EntryCover>,
    /// The track `update_entry_art` last saw as active, to tell a track becoming active again.
    pub entry_art_active: Option<String>,
    /// The picture on the deck, and the one it is changing from while a transition runs.
    pub art_shown: Option<ShownArt>,
    pub art_transition: Option<ArtTransition>,
}

impl NowState {
    /// Put `next` on the deck. Any change to another cover (a file's, a mix track's, the
    /// placeholder) is animated; flipping between the retro and the original picture of the
    /// same cover is not.
    fn show_art(&mut self, next: ShownArt, time: f64) {
        let Some(prev) = self.art_shown.take() else {
            self.art_shown = Some(next);
            return;
        };
        let changed = prev.source != next.source || prev.art.as_ref().map(|t| t.id()) != next.art.as_ref().map(|t| t.id());
        if changed && next.art.is_some() {
            self.art_transition = Some(ArtTransition { from: prev, started: time });
            self.art_shown = Some(next);
        } else if changed {
            self.art_shown = Some(next);
        } else {
            self.art_shown = Some(prev);
        }
    }
}

/// Whether the lookup that failed `failed_for` ago is repeated now.
fn plural(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

fn retry_due(failed_for: Duration, track_reactivated: bool) -> bool {
    track_reactivated || failed_for >= ENTRY_ART_RETRY
}

pub enum EntryCover {
    /// Being looked up since then.
    Looking(Instant),
    /// The services could not be asked (no network) at that moment.
    Failed(Instant),
    Found(EntryArtwork),
}

pub struct EntryArtwork {
    pub art: TextureHandle,
    pub retro_art: Option<TextureHandle>,
    /// The cached image, for the OS media overlay; a generated placeholder has none.
    pub file: Option<PathBuf>,
}

/// Whose cover the deck shows.
#[derive(Clone, Debug, PartialEq)]
pub enum ArtSource {
    /// The playing file's own, or the placeholder when it has none.
    File,
    /// A track of the mix, by `entry_art::key`.
    Entry(String),
}

pub struct ShownArt {
    pub source: ArtSource,
    pub art: Option<TextureHandle>,
    pub retro_art: Option<TextureHandle>,
}

impl ShownArt {
    /// The picture to draw in `mode`: the dithered one when wanted and there is one, and none
    /// at all when the artwork is off.
    pub fn pick(&self, mode: ArtworkMode) -> Option<&TextureHandle> {
        match mode {
            ArtworkMode::Dithered => self.retro_art.as_ref().or(self.art.as_ref()),
            ArtworkMode::Original => self.art.as_ref(),
            ArtworkMode::Off => None,
        }
    }
}

pub struct ArtTransition {
    pub from: ShownArt,
    /// `egui` input time at the start.
    pub started: f64,
}

/// How long the deck keeps the previous track's cover while the next one is looked up, so a
/// quick answer does not flash the mix's cover in between.
const COVER_HOLD: Duration = Duration::from_secs(2);
/// A failed cover lookup is not repeated for this long, or while offline it would run every frame.
const ENTRY_ART_RETRY: Duration = Duration::from_secs(60);

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

/// How far a bulk read of rekordbox data has come, summed over the answers to its chunks.
struct ReadProgress {
    request: u64,
    total: usize,
    done: usize,
    stored: usize,
    waveforms: usize,
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
    /// The sections the user has open. Whether they fit is `plan`'s business, so a window that grows
    /// again brings back what was open.
    pub playlist_open: bool,
    pub setlist_open: bool,
    pub inspector_open: bool,
    /// The section opened last: it wins when only one fits and takes the shared tab.
    recent: Section,
    /// What is drawn, as of the last frame.
    pub plan: Plan,
    /// The LCD and the artwork as last drawn: the parts of the window that dimming leaves bright.
    deck_bright: [egui::Rect; 2],
    /// Whether the window is currently shrunk to just the player (`None` before the
    /// first frame) and the height to return to when it grows back.
    compact: Option<bool>,
    full_height: f32,
    /// The window height last asked for while compact: a narrow window wraps the deck onto more rows.
    compact_height: f32,
    pub message: Option<Message>,
    /// The bulk read of rekordbox data that is running, if any.
    dj_data_read: Option<ReadProgress>,
    pub dialogs: dialogs::Dialogs,
    pub telegram: telegram::State,
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
        file_browser::init(&ctx);
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

        let telegram = telegram::State::new(&store.paths);
        let workers = Workers::start(store.paths.clone(), waker);
        workers.set_rekordbox_key(&settings.rekordbox_key);
        workers.set_mixxx_enabled(settings.mixxx);
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

        // Before the setlist and the inspector had separate switches, one side panel held both as tabs.
        let setlist_open = settings.setlist_open.unwrap_or(settings.side_panel_open);
        let inspector_open = settings.inspector_open.unwrap_or(settings.side_panel_open);
        let mut app = App {
            player,
            engine_events,
            system_events,
            sysmedia: Some(sysmedia),
            workers,
            playlist_open: settings.playlist_open,
            setlist_open,
            inspector_open,
            recent: Section::Playlist,
            plan: Plan::default(),
            deck_bright: [egui::Rect::NOTHING; 2],
            compact: None,
            full_height: DEFAULT_HEIGHT,
            compact_height: 0.0,
            settings,
            store,
            selection: Selection::default(),
            pl_view: Default::default(),
            scroll_to: None,
            now: NowState::default(),
            mix: Default::default(),
            inspector: Default::default(),
            message: None,
            dj_data_read: None,
            dialogs: dialogs::Dialogs::new(),
            telegram,
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
        // Until the first frame measures the window, assume there is room for everything.
        app.plan = sections::plan(app.wanted(), Widths { total: f32::INFINITY, playlist_min: 0.0, side_min: SIDE_MIN_WIDTH }, app.recent);
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
    #[cfg_attr(target_os = "android", allow(dead_code))]
    pub fn install_native_menu(&mut self) {
        #[cfg(target_os = "macos")]
        {
            self.native_menu = menu::NativeMenu::install(self.ctx.clone());
        }
    }

    #[cfg_attr(target_os = "android", allow(dead_code))]
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
    #[cfg_attr(target_os = "android", allow(dead_code))]
    fn open_config_file(&mut self) {
        let path = self.store.paths.config.join("settings.json");
        if !path.exists() {
            self.sync_settings_from_state();
            if let Err(e) = self.store.save_settings(&self.settings) {
                self.error(format!("Cannot create {}: {e}", path.display()));
                return;
            }
        }
        if let Err(e) = dialogs::open_in_editor(&path) {
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

    /// Off, then each dim stage in turn.
    pub fn cycle_dim(&mut self) {
        self.settings.dim = (self.settings.dim + 1) % (theme::DIM_LEVELS + 1);
        self.mark_settings_dirty();
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
        s.playlist_open = self.playlist_open;
        s.setlist_open = Some(self.setlist_open);
        s.inspector_open = Some(self.inspector_open);
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
                if e.info == Default::default() || e.info.bitrate.is_none() { unknown.push(e.path.clone()) } else { known.push(e.path.clone()) }
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

    /// Make a track play another file. The tags of the new file replace what the track was known by; until
    /// they are read the old ones stay. A track that is playing or loading keeps its file.
    pub fn change_entry_file(&mut self, list: usize, entry: EntryId, path: PathBuf) {
        if self.player.is_current(list, entry) && self.player.status != Status::Stopped {
            self.warn("Stop the track before changing its file");
            return;
        }
        let mut changed = false;
        self.player.edit_list(list, |p| {
            if let Some(e) = p.get_mut(entry) {
                (e.path, e.status, changed) = (path.clone(), EntryStatus::Unknown, true);
            }
        });
        if changed {
            self.mark_list_dirty(list);
            self.workers.scan(vec![path.clone()]);
            self.info(format!("The track now plays {}", path.display()));
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
            // Added files are shown even when they do not match the filter.
            self.pl_view.filter.clear();
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

    fn raise_window(&self) {
        self.ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        self.ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        self.ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    /// Files from the command line, a second launch or Finder: added to the list on view, and the first one
    /// plays when they are all plain audio files, the way a double-click is expected to behave.
    fn handle_open_requests(&mut self) {
        let request = crate::open::take();
        if !request.raise {
            return;
        }
        self.raise_window();
        if request.paths.is_empty() {
            return;
        }
        let play = request.paths.iter().all(|p| p.is_file() && crate::workers::is_audio_file(p));
        let list = self.player.viewing;
        self.add_paths(list, request.paths, None);
        if let (true, Some(first)) = (play, self.selection.anchor) {
            self.player.play_entry(list, first, 0.0);
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
                SystemEvent::Raise => self.raise_window(),
                SystemEvent::OpenUri(uri) => match crate::open::uri_to_path(&uri) {
                    Some(path) => crate::open::push(vec![path]),
                    None => log::debug!("cannot open {uri}: not a local file"),
                },
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
        while let Some(request) = file_browser::next_request() {
            if self.dialogs.browser.is_none() {
                self.dialogs.browser = Some(file_browser::Browser::open(request, &self.ctx));
            } else {
                let _ = request.reply.send(None);
            }
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
        self.clear_now_if_unloaded();
        setlist_view::update_active(self);
        self.update_segment_tempo();
        self.update_entry_art();
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
            JobResult::EntryArt { key, result } => self.on_entry_art(key, result),
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
            JobResult::Sticks(sticks) => self.on_sticks(sticks),
            JobResult::CollectionList(result) => match result {
                Ok(overview) => dialogs::open_library_import(self, dialogs::ImportSource::Rekordbox, overview),
                Err(e) => self.error(format!("Cannot read the rekordbox collection: {e}")),
            },
            JobResult::MixxxList(result) => match result {
                Ok(overview) => dialogs::open_library_import(self, dialogs::ImportSource::Mixxx, overview),
                Err(e) => self.error(format!("Cannot read the Mixxx library: {e}")),
            },
            JobResult::Mixxx { read_data, result } => match result {
                Ok(lists) if lists.is_empty() => self.warn("The chosen Mixxx lists have no tracks"),
                Ok(lists) => {
                    let paths: Vec<PathBuf> = lists.iter().filter_map(|l| l.tracks.as_ref().ok()).flatten().map(|(p, _)| p.clone()).collect();
                    log::info!("Mixxx: added {} lists with {} tracks{}", lists.len(), paths.len(), if read_data { ", reading their data" } else { "" });
                    self.on_sticks(lists);
                    if read_data {
                        self.read_dj_data(paths);
                    }
                }
                Err(e) => self.error(format!("Cannot read the Mixxx library: {e}")),
            },
            JobResult::Collection { read_data, result } => match result {
                Ok(lists) if lists.is_empty() => self.warn("The chosen rekordbox playlists have no tracks"),
                Ok(lists) => {
                    let paths = lists.iter().filter_map(|l| l.tracks.as_ref().ok()).flatten().map(|(p, _)| p.clone()).collect();
                    self.on_sticks(lists);
                    if read_data {
                        self.read_dj_data(paths);
                    }
                }
                Err(e) => self.error(format!("Cannot read the rekordbox collection: {e}")),
            },
            JobResult::DjDataRead { request, done, stored, waveforms } => self.on_dj_data_read(request, done, stored, waveforms),
            JobResult::Exported { path, result } => match result {
                Ok(()) => self.info(format!("Exported {}", path.display())),
                Err(e) => self.error(format!("Export failed: {e}")),
            },
            JobResult::NowPlaying(info) => self.on_now_playing(*info),
            JobResult::Waveform { token, result, colors } => {
                if self.player.current.as_ref().is_some_and(|c| c.token == token) {
                    match result {
                        Ok(wave) => {
                            // Colours arriving for a waveform already shown do not sweep it in again.
                            if self.now.waveform.is_none() {
                                self.now.wave_arrived = Some(Instant::now());
                            }
                            self.now.waveform = Some(wave);
                            self.now.wave_colors = colors;
                        }
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
            JobResult::Key { path, result } => inspector::on_key(self, path, result),
            JobResult::SetlistSaved(r) => {
                if let Err(e) = r {
                    self.error(format!("Cannot save setlist: {e}"));
                }
            }
            JobResult::Text { request, path, text } => setlist_view::on_text_loaded(self, request, path, text),
            JobResult::Rediscovered { request, setlist } => setlist_view::on_rediscovered(self, request, setlist),
            JobResult::IdentifyProgress { request, done, total } => setlist_view::on_identify_progress(self, request, done, total),
            JobResult::Identified { request, entry, verdict } => setlist_view::on_identified(self, request, entry, verdict),
            JobResult::IdentifyDone { request, result } => setlist_view::on_identify_done(self, request, result),
            JobResult::Split { request, result } => setlist_view::on_split(self, request, result),
            JobResult::SplitGaps { request, result } => setlist_view::on_split_gaps(self, request, result),
            JobResult::TracklistHits { request, result } => setlist_view::on_tracklist_hits(self, request, result),
            JobResult::TracklistPage { request, result } => setlist_view::on_tracklist_page(self, request, result),
            JobResult::Located { request, root, search, secs } => dialogs::on_located(self, request, root, search, secs),
            JobResult::Spotify { request, result } => spotify::on_playlist(self, request, result),
            JobResult::Telegram(event) => telegram::on_event(self, event),
            JobResult::TelegramThumb { request, index, thumb } => telegram::on_thumb(self, request, index, thumb),
            JobResult::DataPack { export, path, result } => match result {
                Ok(s) if export => self.info(format!(
                    "Saved {}: {}, {} and {}",
                    path.display(),
                    plural(s.setlists, "setlist"),
                    plural(s.waveforms, "waveform"),
                    plural(s.records, "record")
                )),
                Ok(s) => {
                    let mut text = format!("Imported {}, {} and {}", plural(s.setlists + s.merged, "setlist"), plural(s.waveforms, "waveform"), plural(s.records, "record"));
                    if s.present > 0 {
                        text.push_str(&format!("; {} already here", s.present));
                    }
                    if s.rejected > 0 {
                        text.push_str(&format!("; {} skipped as not valid", s.rejected));
                    }
                    if s.setlists + s.merged > 0 {
                        text.push_str(". Open a mix again to see its setlist");
                    }
                    self.info(text);
                }
                Err(e) => self.error(format!("{}: {e}", path.display())),
            },
            JobResult::TextWritten { path, result } => match result {
                Ok(()) => self.info(format!("Saved {}", path.display())),
                Err(e) => self.error(format!("Cannot write {}: {e}", path.display())),
            },
            JobResult::Inspected { .. }
            | JobResult::Image { .. }
            | JobResult::Written { .. }
            | JobResult::TrackCover { .. }
            | JobResult::Restored { .. } => inspector::on_result(self, r),
        }
    }

    /// Reads what rekordbox and Mixxx know about `paths` (waveform, key, BPM, rating, colour) into the
    /// cache, so it is there however those files are played later. Files that are in neither are skipped.
    pub fn read_dj_data(&mut self, paths: Vec<PathBuf>) {
        let mut seen = HashSet::new();
        let paths: Vec<PathBuf> = paths.into_iter().filter(|p| seen.insert(p.clone())).collect();
        if paths.is_empty() {
            return;
        }
        let total = paths.len();
        log::info!("DJ software data: reading {total} files");
        let request = self.workers.read_dj_data(paths);
        self.dj_data_read = Some(ReadProgress { request, total, done: 0, stored: 0, waveforms: 0 });
    }

    fn on_dj_data_read(&mut self, request: u64, done: usize, stored: usize, waveforms: usize) {
        let Some(progress) = self.dj_data_read.as_mut().filter(|p| p.request == request) else { return };
        progress.done += done;
        progress.stored += stored;
        progress.waveforms += waveforms;
        if progress.done >= progress.total {
            let (total, stored, waveforms) = (progress.total, progress.stored, progress.waveforms);
            self.dj_data_read = None;
            let message = format!("Read DJ software data for {stored} of {total} tracks ({waveforms} with a waveform)");
            log::info!("{message}");
            self.info(message);
        }
    }

    /// Each stick becomes a playlist named after its volume; opening it again refreshes that playlist.
    fn on_sticks(&mut self, sticks: Vec<crate::rekordbox::Loaded>) {
        if sticks.is_empty() {
            self.warn("No rekordbox USB stick found");
            return;
        }
        for stick in sticks {
            let entries = match stick.tracks {
                Ok(tracks) => tracks.into_iter().map(|(path, info)| PlaylistEntry::with_info(path, info)).collect::<Vec<_>>(),
                Err(e) => {
                    self.error(format!("Cannot read the rekordbox library on {}: {e}", stick.name));
                    continue;
                }
            };
            let list = match self.player.lists.iter().position(|d| d.playlist.name == stick.name) {
                Some(i) => {
                    self.player.edit_list(i, |p| p.clear());
                    i
                }
                None => {
                    playlist_view::new_playlist(self, &stick.name);
                    self.player.viewing
                }
            };
            self.player.viewing = list;
            let paths: Vec<PathBuf> = entries.iter().map(|e| e.path.clone()).collect();
            self.insert_entries(list, entries, None);
            self.workers.check_exists(paths);
        }
    }

    /// With no file loaded (its list was cleared or removed) the deck falls back to the empty
    /// state, and once playback is stopped it drops the waveform and cover of the file, which
    /// load again when it plays. Either way the last file's picture does not linger.
    fn clear_now_if_unloaded(&mut self) {
        if self.player.current_entry().is_none() {
            if self.now.waveform.is_some() || self.now.meta.is_some() || self.now.art_shown.is_some() {
                self.now = NowState::default();
            }
        } else if self.player.status == Status::Stopped {
            let n = &mut self.now;
            (n.waveform, n.wave_colors, n.wave_arrived, n.art, n.retro_art, n.art_file, n.art_shown, n.art_transition) = Default::default();
        }
    }

    /// The original artwork shows band colours, which a waveform decoded under another setting lacks:
    /// ask for them for the playing track. The waveform on screen stays until they arrive.
    pub fn request_wave_colors(&mut self) {
        if let Some(c) = self.player.current.as_ref().filter(|_| self.now.wave_colors.is_none()) {
            self.workers.request_waveform(c.token, c.path.clone(), self.settings.rekordbox_waveform, true);
        }
    }

    fn on_track_changed(&mut self) {
        let Some(c) = self.player.current.clone() else { return };
        self.mark_settings_dirty();
        // The deck keeps its picture until the new file's cover arrives, to change from it.
        let (art_shown, art_transition) = (self.now.art_shown.take(), self.now.art_transition.take());
        self.now = NowState { art_shown, art_transition, ..NowState::default() };
        setlist_view::reset_for_track(self, c.token, c.path.clone());
        self.workers.submit(Job::NowPlaying { token: c.token, path: c.path.clone() });
        self.workers.request_waveform(c.token, c.path.clone(), self.settings.rekordbox_waveform, self.settings.artwork == ArtworkMode::Original);
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

    /// The lookup key of the active track of the playing mix, when it names one.
    pub(crate) fn active_entry_key(&self) -> Option<(String, &nullamp_core::SetlistEntry)> {
        let (setlist, i) = setlist_view::shown(self).zip(self.mix.active)?;
        let e = setlist.entries.get(i)?;
        if e.artist.trim().is_empty() || e.title.trim().is_empty() || e.is_unidentified() {
            return None;
        }
        Some((crate::entry_art::key(&e.artist, &e.full_title()), e))
    }

    /// Ask for the cover of the active track of a mix the first time it becomes active.
    fn update_entry_art(&mut self) {
        if !self.settings.online_artwork {
            return;
        }
        let Some((key, e)) = self.active_entry_key() else {
            self.now.entry_art_active = None;
            return;
        };
        let (artist, title) = (e.artist.trim().to_owned(), e.full_title());
        let reactivated = self.now.entry_art_active.as_deref() != Some(key.as_str());
        self.now.entry_art_active = Some(key.clone());
        match self.now.entry_art.get(&key) {
            None => {}
            Some(EntryCover::Failed(since)) if retry_due(since.elapsed(), reactivated) => {}
            Some(_) => return,
        }
        self.now.entry_art.insert(key.clone(), EntryCover::Looking(Instant::now()));
        self.workers.request_entry_art(key, artist, title);
    }

    pub(crate) fn on_entry_art(&mut self, key: String, result: Result<crate::workers::EntryArt, String>) {
        // Answers for the tracks of a mix that is no longer playing are dropped.
        if !self.now.entry_art.contains_key(&key) {
            return;
        }
        match result {
            Ok(found) => {
                let texture = |name: &str, t: &crate::workers::Thumb| {
                    self.ctx.load_texture(name, egui::ColorImage::from_rgba_unmultiplied(t.size, &t.rgba), egui::TextureOptions::LINEAR)
                };
                let cover = EntryArtwork {
                    art: texture("entry-art", &found.art),
                    retro_art: found.retro_art.as_ref().map(|t| texture("entry-art-retro", t)),
                    file: found.file,
                };
                self.now.entry_art.insert(key, EntryCover::Found(cover));
            }
            Err(e) => {
                log::warn!("artwork lookup failed: {e}");
                // Asked again after a pause, or when the track next becomes active.
                self.now.entry_art.insert(key, EntryCover::Failed(Instant::now()));
            }
        }
    }

    fn active_entry_cover(&self) -> Option<(String, &EntryCover)> {
        let (key, _) = self.active_entry_key()?;
        let cover = self.now.entry_art.get(&key)?;
        Some((key, cover))
    }

    fn active_entry_artwork(&self) -> Option<&EntryArtwork> {
        match self.active_entry_cover()? {
            (_, EntryCover::Found(found)) => Some(found),
            _ => None,
        }
    }

    /// The deck's cover: the active track's while a mix plays and its cover has arrived, else the
    /// file's. `None` for a moment while the active track's cover is being looked up or the new
    /// file's is being read: the deck keeps what it shows.
    pub fn deck_art(&self) -> Option<ShownArt> {
        match self.active_entry_cover() {
            Some((key, EntryCover::Found(found))) => {
                return Some(ShownArt { source: ArtSource::Entry(key), art: Some(found.art.clone()), retro_art: found.retro_art.clone() });
            }
            Some((_, EntryCover::Looking(since))) if since.elapsed() < COVER_HOLD && self.now.art_shown.is_some() => return None,
            _ => {}
        }
        if self.now.art.is_none() && self.now.art_shown.is_some() {
            return None;
        }
        Some(ShownArt { source: ArtSource::File, art: self.now.art.clone(), retro_art: self.now.retro_art.clone() })
    }

    /// Update the deck's picture, starting a transition when it changes and ending one that has run its course.
    pub fn refresh_deck_art(&mut self, time: f64) {
        if let Some(next) = self.deck_art() {
            self.now.show_art(next, time);
        }
        if self.now.art_transition.as_ref().is_some_and(|t| time - t.started >= deck::ART_TRANSITION_SECS) {
            self.now.art_transition = None;
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
                let ti = info.track_info.clone().unwrap_or_else(|| m.track_info());
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
        if let Some(file) = self.active_entry_artwork().and_then(|found| found.file.clone()) {
            np.cover = Some(file);
        }
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
            self.toggle_side(Section::Inspector);
        }
        if consume(cmd(Key::L)) {
            self.toggle_side(Section::Setlist);
        }
        if consume(cmd(Key::P)) {
            self.toggle_playlist();
        }
        if consume(cmd(Key::J)) {
            playlist_view::show_playing(self);
        }
        if consume(cmd(Key::F)) {
            if !self.plan.playlist {
                self.toggle_playlist();
            }
            self.pl_view.filter.focus = true;
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
        if pressed(Key::D) {
            self.cycle_dim();
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
        if self.plan.playlist {
            playlist_view::handle_keys(self, ctx);
        }
    }

    pub fn toggle_playlist(&mut self) {
        if self.plan.playlist {
            self.playlist_open = false;
        } else {
            self.show_playlist();
        }
        self.mark_settings_dirty();
    }

    /// Opens the playlist; in a narrow window it takes the place of the section showing.
    pub fn show_playlist(&mut self) {
        self.playlist_open = true;
        self.recent = Section::Playlist;
    }

    /// Opens the setlist or the inspector; in a narrow window it takes the place of the section showing.
    pub fn show_side(&mut self, section: Section) {
        match section {
            Section::Setlist => self.setlist_open = true,
            Section::Inspector => self.inspector_open = true,
            Section::Playlist => return self.show_playlist(),
        }
        self.recent = section;
    }

    fn wanted(&self) -> Wanted {
        // A single track has no setlist to show; the switch is kept for the next mix.
        Wanted { playlist: self.playlist_open, setlist: self.setlist_open && self.mix.is_mix, inspector: self.inspector_open }
    }

    /// Decides what the window has room for. Sections that do not fit stay open in the background and
    /// return when the window is wide enough again.
    fn fit_sections(&mut self, width: f32) {
        if width <= 0.0 {
            return;
        }
        let widths = Widths { total: width, playlist_min: self.pl_view.toolbar_min, side_min: SIDE_MIN_WIDTH };
        self.plan = sections::plan(self.wanted(), widths, self.recent);
        // A phone is never just the player: closing the last section brings the playlist back.
        if cfg!(target_os = "android") && !self.plan.any() {
            self.show_playlist();
            self.plan = sections::plan(self.wanted(), widths, self.recent);
        }
    }

    /// With nothing below the deck showing, the window is just the
    /// deck and the status bar, so shrink it to that and restore it afterwards.
    fn fit_window(&mut self, ctx: &egui::Context, deck_height: f32) {
        let compact = !self.plan.any();
        let before = self.compact.replace(compact);
        let height = (deck_height + if self.settings.show_status_bar { STATUS_HEIGHT } else { 0.0 }).ceil();
        if before == Some(compact) && (!compact || height == self.compact_height) {
            return;
        }
        let size = ctx.content_rect().size();
        if compact {
            if before == Some(false) {
                self.full_height = size.y;
            }
            self.compact_height = height;
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(vec2(MIN_WIDTH, height)));
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(vec2(size.x.max(MIN_WIDTH), height)));
        } else {
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(vec2(MIN_WIDTH, MIN_FULL_HEIGHT)));
            if before.is_some() {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(vec2(size.x, self.full_height)));
            }
        }
    }

    pub fn toggle_side(&mut self, section: Section) {
        if self.plan.shows(section) {
            match section {
                Section::Setlist => self.setlist_open = false,
                Section::Inspector => self.inspector_open = false,
                Section::Playlist => self.playlist_open = false,
            }
        } else {
            if section == Section::Setlist && !self.mix.is_mix {
                // Only a playing track can be given a setlist.
                if self.player.current.is_none() {
                    return;
                }
                setlist_view::pin(self);
            }
            self.show_side(section);
            if section == Section::Inspector && self.inspector.targets.is_empty() {
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

    /// A setlist or inspector column. Where the two share a column, its header switches between them.
    fn side_panel(&mut self, ui: &mut egui::Ui, section: Section) {
        ui.horizontal(|ui| {
            let tabs: &[(Section, &str)] = if self.plan.tabs { &[(Section::Setlist, "Setlist"), (Section::Inspector, "Inspector")] } else if section == Section::Setlist { &[(Section::Setlist, "Setlist")] } else { &[(Section::Inspector, "Inspector")] };
            for &(tab, label) in tabs {
                if widgets::tab(ui, tab == section, label).clicked() && tab != section {
                    self.recent = tab;
                    if tab == Section::Inspector && self.inspector.targets.is_empty() {
                        inspector::load_on_open(self);
                    }
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("✕").on_hover_text("Hide panel").clicked() {
                    self.toggle_side(section);
                }
            });
        });
        ui.add_space(4.0);
        match section {
            Section::Setlist => setlist_view::show(self, ui),
            _ => inspector::show(self, ui),
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_centered(|ui| {
            let shown = playlist_view::shown_count(self);
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
            if let Some(n) = shown {
                ui.label(egui::RichText::new(format!("· {n} shown")).color(theme::text_dim()).size(11.5));
            }
            if !self.selection.ids.is_empty() {
                ui.label(egui::RichText::new(format!("· {} selected", self.selection.ids.len())).color(theme::text_dim()).size(11.5));
            }
            if missing > 0 {
                ui.label(egui::RichText::new(format!("· {missing} unavailable")).color(theme::warn()).size(11.5));
            }
            if let Some(r) = &self.dj_data_read {
                ui.label(egui::RichText::new(format!("· reading DJ software data {}/{}", r.done, r.total)).color(theme::text_dim()).size(11.5));
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
                // Android has no media service to reach, and a narrow window has no room beside the other texts.
                let room = ui.available_width() >= MEDIA_KEYS_NOTE_WIDTH;
                if room && !cfg!(target_os = "android") && self.sysmedia.as_ref().is_some_and(|s| !s.is_available()) {
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
        self.handle_open_requests();
        self.schedule_repaint(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        theme::sync(&ctx);
        self.fit_sections(ctx.content_rect().width());
        self.handle_shortcuts(&ctx);
        self.handle_dropped_files(&ctx);

        // Where system bars cover the window (Android), the panels at the edges pad themselves clear of them.
        let (bars_top, bars_bottom) = crate::system_bar_insets(ctx.pixels_per_point());
        let deck = egui::Panel::top("deck")
            .frame(egui::Frame::new().fill(theme::deck_bg()).inner_margin(egui::Margin { left: 10, right: 10, top: 8 + bars_top as i8, bottom: 8 }))
            .resizable(false)
            .show(ui, |ui| theme::with_dark(ui, |ui| deck::show(self, ui)));
        self.fit_window(&ctx, deck.response.rect.height());
        // Hidden, the strip still keeps clear of the gesture bar where there is one.
        let status_height = if self.settings.show_status_bar { STATUS_HEIGHT } else { 0.0 };
        if status_height + bars_bottom > 0.0 {
            egui::Panel::bottom("status")
                .frame(egui::Frame::new().fill(theme::bg()).inner_margin(egui::Margin { left: 10, right: 10, top: 3, bottom: 3 + bars_bottom as i8 }))
                .exact_size(status_height + bars_bottom)
                .resizable(false)
                .show(ui, |ui| {
                    if self.settings.show_status_bar {
                        self.status_bar(ui)
                    }
                });
        }
        // The setlist and the inspector sit right of the playlist. Without the playlist the first of them
        // fills the window and the other is a panel beside it. Panels are added from the outermost in.
        let plan = self.plan;
        let sides: Vec<Section> = [Section::Setlist, Section::Inspector].into_iter().filter(|&s| plan.shows(s)).collect();
        let (fill, panels) = match sides.split_first() {
            Some((&first, rest)) if !plan.playlist => (Some(first), rest.to_vec()),
            _ => (None, sides),
        };
        let total = ctx.content_rect().width();
        let mut used = 0.0;
        for (i, &section) in panels.iter().rev().enumerate() {
            let left_of = if plan.playlist { self.pl_view.toolbar_min } else { SIDE_MIN_WIDTH } + (panels.len() - 1 - i) as f32 * SIDE_MIN_WIDTH;
            let shown = egui::Panel::right(if i == 0 { "side" } else { "side_left" })
                .frame(egui::Frame::new().fill(theme::panel()).inner_margin(egui::Margin::same(8)))
                .default_size(380.0)
                .size_range(SIDE_MIN_WIDTH..=(total - used - left_of).clamp(SIDE_MIN_WIDTH, 720.0))
                .resizable(true)
                .show(ui, |ui| self.side_panel(ui, section));
            used += shown.response.rect.width();
        }
        let central = egui::CentralPanel::default();
        if plan.playlist {
            central
                .frame(egui::Frame::new().fill(theme::panel()).inner_margin(egui::Margin::same(0)))
                .show(ui, |ui| playlist_view::show(self, ui));
        } else if let Some(section) = fill {
            central.frame(egui::Frame::new().fill(theme::panel()).inner_margin(egui::Margin::same(8))).show(ui, |ui| self.side_panel(ui, section));
        } else {
            central.frame(egui::Frame::new().fill(theme::bg()).inner_margin(egui::Margin::same(0))).show(ui, |_| {});
        }

        // A veil painted last on the root layer, so dialogs and popups (higher layers) stay bright.
        if let Some(veil) = theme::dim_veil(self.settings.dim) {
            let [lcd, art] = self.deck_bright;
            let all = ctx.content_rect();
            let p = ui.painter();
            // The LCD and the artwork share a row; the veil is everything around them.
            for r in [
                egui::Rect::from_min_max(all.min, egui::pos2(all.right(), lcd.top())),
                egui::Rect::from_min_max(egui::pos2(all.left(), lcd.bottom()), all.max),
                egui::Rect::from_min_max(egui::pos2(all.left(), lcd.top()), egui::pos2(lcd.left(), lcd.bottom())),
                egui::Rect::from_min_max(egui::pos2(lcd.right(), lcd.top()), egui::pos2(art.left(), lcd.bottom())),
                egui::Rect::from_min_max(egui::pos2(art.right(), lcd.top()), egui::pos2(all.right(), lcd.bottom())),
            ] {
                p.rect_filled(r, 0.0, veil);
            }
        }

        dialogs::show_modals(self, &ctx);
        dialogs::show_deck_overlay(self, &ctx, deck.response.rect);
        dialogs::show_browser(self, &ctx);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_cover_lookup_waits_unless_the_track_becomes_active_again() {
        assert!(!retry_due(Duration::from_millis(40), false), "offline, the next frame must not ask again");
        assert!(!retry_due(ENTRY_ART_RETRY - Duration::from_secs(1), false));
        assert!(retry_due(ENTRY_ART_RETRY, false));
        assert!(retry_due(Duration::ZERO, true));
    }

    /// "Change file…" points one track at another file and has the new file's tags read; a track that is
    /// playing keeps its file, and a track that is gone changes nothing.
    #[test]
    fn a_track_can_be_pointed_at_another_file() {
        use crate::ui_snapshots::{seeded_store, settle};
        let dir = tempfile::tempdir().unwrap();
        let store = seeded_store(dir.path(), "dark");
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1180.0, 740.0)).build_eframe(|cc| App::new(cc, store));
        settle(&mut h, 300);
        let app = h.state_mut();
        let entries = app.player.lists[0].playlist.entries();
        let (gone, other) = (entries[6].id, entries[1].path.clone());
        assert_eq!(app.player.lists[0].playlist.get(gone).unwrap().path.file_name().unwrap(), "missing.mp3");
        app.player.lists[0].playlist.get_mut(gone).unwrap().status = EntryStatus::Missing;
        let old_info = app.player.lists[0].playlist.get(gone).unwrap().info.clone();

        app.change_entry_file(0, gone, other.clone());
        let track = app.player.lists[0].playlist.get(gone).unwrap();
        assert_eq!((&track.path, &track.status), (&other, &EntryStatus::Unknown));
        assert_eq!(track.info, old_info, "the old tags stay until the new file's are read");
        settle(&mut h, 500);
        let track = h.state().player.lists[0].playlist.get(gone).unwrap();
        assert_eq!(track.info.title.as_deref(), Some("V24 Tone"), "the new file's tags replace them");
        assert_eq!(h.state().player.lists[0].playlist.entries()[1].path, other, "the other track with that file is untouched");

        let app = h.state_mut();
        let before: Vec<PathBuf> = app.player.lists[0].playlist.entries().iter().map(|e| e.path.clone()).collect();
        app.change_entry_file(0, EntryId::new(), other.clone());
        let after: Vec<PathBuf> = app.player.lists[0].playlist.entries().iter().map(|e| e.path.clone()).collect();
        assert_eq!(before, after);
    }
}
