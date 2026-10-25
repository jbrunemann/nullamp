//! Native file dialogs (run off the UI thread) and in-app modal dialogs.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crossbeam_channel::{Receiver, Sender};
use egui::{Key, RichText};

use super::{App, list_index, playlist_view, setlist_view, theme, widgets};
use crate::cache::{self, Kind, Usage};
use crate::rekordbox::Overview;
use crate::workers::{AUDIO_EXTENSIONS, Job};

pub enum DialogResult {
    AddFiles { list: String, paths: Vec<PathBuf> },
    ImportPlaylist { list: String, path: PathBuf },
    ExportPlaylist { list: String, path: PathBuf },
    ImportSetlist(PathBuf),
    ExportSetlist { format: setlist_view::ExportFormat, path: PathBuf },
    Artwork(PathBuf),
    /// The folder chosen on the Telegram tab for the files the bot sends.
    TelegramDir(PathBuf),
    UpdateChecked(Result<nullamp_update::Status, nullamp_update::Error>),
    /// What the cache folder holds, after a scan or a clear.
    CacheUsage(Vec<(Kind, Usage)>),
    /// The version of the Python 3 found on this machine, `None` when there is none.
    Python(Option<String>),
}

/// The pages of the settings screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsTab {
    General,
    Audio,
    Shortcuts,
    About,
    Logs,
    Updates,
    Cache,
    Rekordbox,
    Web,
    Telegram,
}

impl SettingsTab {
    const ALL: [SettingsTab; 10] = [SettingsTab::General, SettingsTab::Audio, SettingsTab::Shortcuts, SettingsTab::Rekordbox, SettingsTab::Web, SettingsTab::Telegram, SettingsTab::Updates, SettingsTab::Cache, SettingsTab::Logs, SettingsTab::About];

    fn label(self) -> &'static str {
        match self {
            SettingsTab::General => "General",
            SettingsTab::Audio => "Audio device",
            SettingsTab::Shortcuts => "Shortcuts",
            SettingsTab::About => "About",
            SettingsTab::Logs => "Logs",
            SettingsTab::Updates => "Updates",
            SettingsTab::Cache => "Cache",
            SettingsTab::Rekordbox => "rekordbox",
            SettingsTab::Web => "Web",
            SettingsTab::Telegram => "Telegram",
        }
    }
}

/// Where the update check stands. It only runs when the user asks.
#[derive(Debug)]
pub enum UpdateState {
    Idle,
    Checking,
    Done(Result<nullamp_update::Status, nullamp_update::Error>),
}

/// Whether a Python 3 is installed, for the rekordbox key recipe. Looked for once when the rekordbox
/// tab is first shown (it starts a process), again on request.
#[derive(Debug, Default, PartialEq)]
pub enum PythonState {
    #[default]
    Unknown,
    Checking,
    Found(String),
    Missing,
}

/// Which library the chooser lists.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ImportSource {
    Rekordbox,
    Mixxx,
}

/// The choice of what to add from the rekordbox desktop library or the Mixxx library, while its dialog is open.
pub struct LibraryImport {
    pub source: ImportSource,
    pub overview: Overview,
    /// The whole collection as one list.
    pub all: bool,
    /// Ids of the playlists ticked.
    pub selected: HashSet<String>,
}

/// What the Cache tab shows. The folder changes while tracks play, so it is read again on each visit.
#[derive(Default)]
pub struct CacheState {
    pub usage: Option<Vec<(Kind, Usage)>>,
    /// A scan or a clear is running.
    busy: bool,
    /// This visit to the tab has asked for the numbers.
    requested: bool,
}

pub struct Dialogs {
    tx: Sender<DialogResult>,
    pub results: Receiver<DialogResult>,
    pub rename: Option<(usize, String)>,
    pub new_playlist: Option<String>,
    pub confirm_clear: Option<usize>,
    pub confirm_delete: Option<usize>,
    /// The settings page taking over the deck, on this tab.
    pub settings: Option<SettingsTab>,
    about_icon: Option<egui::TextureHandle>,
    pub update: UpdateState,
    pub cache: CacheState,
    pub python: PythonState,
    pub library_import: Option<LibraryImport>,
}

impl Dialogs {
    pub fn new() -> Self {
        let (tx, results) = crossbeam_channel::unbounded();
        Dialogs { tx, results, rename: None, new_playlist: None, confirm_clear: None, confirm_delete: None, settings: None, about_icon: None, update: UpdateState::Idle, cache: CacheState::default(), python: PythonState::Unknown, library_import: None }
    }
}

/// Android has no file dialog yet. This stands in for `rfd` there: it picks nothing, except that "Add folder"
/// yields the music folders: the app's own `Music` folder (`adb push` files into it) and the shared one.
#[cfg(target_os = "android")]
mod rfd {
    use std::path::{Path, PathBuf};

    pub struct FileHandle(PathBuf);

    impl FileHandle {
        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    pub struct AsyncFileDialog;

    impl AsyncFileDialog {
        pub fn new() -> Self {
            AsyncFileDialog
        }

        pub fn set_title(self, _: impl Into<String>) -> Self {
            self
        }

        pub fn add_filter(self, _: impl Into<String>, _: &[&str]) -> Self {
            self
        }

        pub fn set_directory(self, _: impl AsRef<Path>) -> Self {
            self
        }

        pub fn set_file_name(self, _: impl Into<String>) -> Self {
            self
        }

        pub async fn pick_folders(self) -> Option<Vec<FileHandle>> {
            let own = std::env::var_os("NULLAMP_MUSIC").map(PathBuf::from);
            let dirs: Vec<FileHandle> = own.into_iter().chain([PathBuf::from("/storage/emulated/0/Music")]).filter(|d| {
                let found = d.is_dir();
                log::info!("music folder {}: {}", d.display(), if found { "found" } else { "missing or unreadable" });
                found
            }).map(FileHandle).collect();
            (!dirs.is_empty()).then_some(dirs)
        }

        pub async fn pick_folder(self) -> Option<FileHandle> {
            None
        }

        pub async fn pick_files(self) -> Option<Vec<FileHandle>> {
            None
        }

        pub async fn pick_file(self) -> Option<FileHandle> {
            None
        }

        pub async fn save_file(self) -> Option<FileHandle> {
            None
        }
    }
}

/// Run an async rfd dialog on a helper thread so the UI loop (and with it media keys
/// and track changes) keeps running while the dialog is open.
fn spawn<F, Fut>(app: &App, make: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Option<DialogResult>>,
{
    let tx = app.dialogs.tx.clone();
    let ctx = app.ctx.clone();
    std::thread::spawn(move || {
        if let Some(r) = pollster::block_on(make()) {
            let _ = tx.send(r);
            ctx.request_repaint();
        }
    });
}

/// Ask the update address for the newest release on a helper thread (it runs curl and may take
/// seconds). Ignored while a check is running.
pub fn check_for_updates(app: &mut App) {
    if matches!(app.dialogs.update, UpdateState::Checking) {
        return;
    }
    app.dialogs.update = UpdateState::Checking;
    let (tx, ctx, source) = (app.dialogs.tx.clone(), app.ctx.clone(), app.settings.update_url.clone());
    std::thread::spawn(move || {
        let version = env!("CARGO_PKG_VERSION");
        let _ = tx.send(DialogResult::UpdateChecked(nullamp_update::check(&source, version, &format!("nullamp/{version}"))));
        ctx.request_repaint();
    });
}

/// Look for a Python 3 on a helper thread (it starts a process). The answer is `DialogResult::Python`.
fn check_python(app: &mut App) {
    app.dialogs.python = PythonState::Checking;
    let (tx, ctx) = (app.dialogs.tx.clone(), app.ctx.clone());
    std::thread::spawn(move || {
        let _ = tx.send(DialogResult::Python(crate::python::detect()));
        ctx.request_repaint();
    });
}

/// Shows the playlists of a library for the user to choose from.
pub fn open_library_import(app: &mut App, source: ImportSource, overview: Overview) {
    app.show_playlist();
    app.dialogs.library_import = Some(LibraryImport { source, overview, all: false, selected: HashSet::new() });
}

/// "Open Mixxx library…": turns the Mixxx reader on, which from then on also fills in the key and
/// BPM of files in the library, and lists what the library holds to choose from.
pub fn open_mixxx(app: &mut App) {
    if !app.settings.mixxx {
        app.settings.mixxx = true;
        app.mark_settings_dirty();
    }
    app.workers.set_mixxx_enabled(true);
    app.workers.submit(Job::ListMixxx);
}

/// Read the cache folder, after removing the files of `clear`, on a helper thread (it walks the
/// folder). The answer is the new `CacheUsage`.
fn scan_cache(app: &mut App, clear: Vec<Kind>) {
    app.dialogs.cache.busy = true;
    app.dialogs.cache.requested = true;
    let (tx, ctx, paths) = (app.dialogs.tx.clone(), app.ctx.clone(), app.store.paths.clone());
    std::thread::spawn(move || {
        if !clear.is_empty() {
            let removed = cache::clear(&paths, &clear);
            log::info!("cleared {removed} cache files");
        }
        let _ = tx.send(DialogResult::CacheUsage(cache::scan(&paths)));
        ctx.request_repaint();
    });
}

/// Choose the folder the files from Telegram are saved in.
pub fn choose_telegram_dir(app: &mut App) {
    let start = app.settings.telegram_dir.clone().filter(|d| d.is_dir()).or_else(|| start_dir(app));
    spawn(app, move || async move {
        let mut d = rfd::AsyncFileDialog::new().set_title("Save files from Telegram in");
        if let Some(dir) = start {
            d = d.set_directory(dir);
        }
        Some(DialogResult::TelegramDir(d.pick_folder().await?.path().to_path_buf()))
    });
}

fn start_dir(app: &App) -> Option<PathBuf> {
    app.settings.last_dir.clone().filter(|d| d.is_dir())
}

pub fn open_files(app: &mut App) {
    let list = app.player.lists[app.player.viewing].key.clone();
    let dir = start_dir(app);
    spawn(app, move || async move {
        let mut d = rfd::AsyncFileDialog::new().set_title("Add audio files").add_filter("Audio", AUDIO_EXTENSIONS).add_filter("Playlists", &["m3u", "m3u8"]);
        if let Some(dir) = dir {
            d = d.set_directory(dir);
        }
        let files = d.pick_files().await?;
        Some(DialogResult::AddFiles { list, paths: files.into_iter().map(|f| f.path().to_path_buf()).collect() })
    });
}

pub fn open_folder(app: &mut App) {
    let list = app.player.lists[app.player.viewing].key.clone();
    let dir = start_dir(app);
    spawn(app, move || async move {
        let mut d = rfd::AsyncFileDialog::new().set_title("Add folder");
        if let Some(dir) = dir {
            d = d.set_directory(dir);
        }
        let folders = d.pick_folders().await?;
        Some(DialogResult::AddFiles { list, paths: folders.into_iter().map(|f| f.path().to_path_buf()).collect() })
    });
}

pub fn import_playlist(app: &mut App) {
    let list = app.player.lists[app.player.viewing].key.clone();
    let dir = start_dir(app);
    spawn(app, move || async move {
        let mut d = rfd::AsyncFileDialog::new().set_title("Import playlist").add_filter("Playlists", &["m3u", "m3u8"]);
        if let Some(dir) = dir {
            d = d.set_directory(dir);
        }
        let f = d.pick_file().await?;
        Some(DialogResult::ImportPlaylist { list, path: f.path().to_path_buf() })
    });
}

pub fn export_playlist(app: &mut App, index: usize) {
    let doc = &app.player.lists[index];
    let list = doc.key.clone();
    let name = format!("{}.m3u8", sanitize(&doc.playlist.name));
    let dir = start_dir(app);
    spawn(app, move || async move {
        let mut d = rfd::AsyncFileDialog::new().set_title("Export playlist").set_file_name(name).add_filter("M3U8 playlist", &["m3u8", "m3u"]);
        if let Some(dir) = dir {
            d = d.set_directory(dir);
        }
        let f = d.save_file().await?;
        Some(DialogResult::ExportPlaylist { list, path: f.path().to_path_buf() })
    });
}

pub fn import_setlist(app: &mut App) {
    let dir = app.player.current.as_ref().and_then(|c| c.path.parent().map(Path::to_path_buf));
    spawn(app, move || async move {
        let mut d = rfd::AsyncFileDialog::new()
            .set_title("Import setlist")
            .add_filter("Cue sheets and tracklists", &["cue", "txt", "nfo", "md"]);
        if let Some(dir) = dir {
            d = d.set_directory(dir);
        }
        let f = d.pick_file().await?;
        Some(DialogResult::ImportSetlist(f.path().to_path_buf()))
    });
}

pub fn export_setlist(app: &mut App, format: setlist_view::ExportFormat) {
    let Some(c) = app.player.current.as_ref() else { return };
    let stem = c.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "setlist".into());
    let dir = c.path.parent().map(Path::to_path_buf);
    let (ext, label) = match format {
        setlist_view::ExportFormat::Cue => ("cue", "Cue sheet"),
        setlist_view::ExportFormat::Text => ("txt", "Text tracklist"),
    };
    let name = format!("{stem}.{ext}");
    spawn(app, move || async move {
        let mut d = rfd::AsyncFileDialog::new().set_title("Export setlist").set_file_name(name).add_filter(label, &[ext]);
        if let Some(dir) = dir {
            d = d.set_directory(dir);
        }
        let f = d.save_file().await?;
        Some(DialogResult::ExportSetlist { format, path: f.path().to_path_buf() })
    });
}

pub fn pick_artwork(app: &mut App) {
    let dir = start_dir(app);
    spawn(app, move || async move {
        let mut d = rfd::AsyncFileDialog::new().set_title("Choose artwork").add_filter("Images", &["jpg", "jpeg", "png", "gif", "webp", "bmp"]);
        if let Some(dir) = dir {
            d = d.set_directory(dir);
        }
        let f = d.pick_file().await?;
        Some(DialogResult::Artwork(f.path().to_path_buf()))
    });
}

fn sanitize(name: &str) -> String {
    name.chars().map(|c| if "/\\:*?\"<>|".contains(c) { '_' } else { c }).collect()
}

pub fn on_dialog_result(app: &mut App, r: DialogResult) {
    let remember_dir = |app: &mut App, p: &Path| {
        let dir = if p.is_dir() { Some(p.to_path_buf()) } else { p.parent().map(Path::to_path_buf) };
        if dir.is_some() {
            app.settings.last_dir = dir;
            app.mark_settings_dirty();
        }
    };
    match r {
        DialogResult::AddFiles { list, paths } => {
            if let Some(p) = paths.first() {
                remember_dir(app, p);
            }
            if let Some(i) = list_index(app, &list) {
                app.add_paths(i, paths, None);
            }
        }
        DialogResult::ImportPlaylist { list, path } => {
            remember_dir(app, &path);
            if let Some(i) = list_index(app, &list) {
                app.add_paths(i, vec![path], None);
            }
        }
        DialogResult::ExportPlaylist { list, path } => {
            remember_dir(app, &path);
            if let Some(i) = list_index(app, &list) {
                let items = app.player.lists[i].playlist.entries().iter().map(|e| (e.path.clone(), e.info.clone())).collect();
                app.workers.submit(Job::ExportPlaylist { path, items });
            }
        }
        DialogResult::UpdateChecked(result) => app.dialogs.update = UpdateState::Done(result),
        DialogResult::CacheUsage(usage) => {
            app.dialogs.cache.usage = Some(usage);
            app.dialogs.cache.busy = false;
        }
        DialogResult::Python(found) => app.dialogs.python = found.map_or(PythonState::Missing, PythonState::Found),
        DialogResult::ImportSetlist(path) => setlist_view::import_file(app, path),
        DialogResult::ExportSetlist { format, path } => setlist_view::export_to(app, format, path),
        DialogResult::Artwork(path) => super::inspector::add_artwork_file(app, path),
        DialogResult::TelegramDir(dir) => {
            app.settings.telegram_dir = Some(dir);
            app.mark_settings_dirty();
        }
    }
}

/// Reveal a file in the platform's file manager.
pub fn reveal(path: &Path) {
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg("-R").arg(path).spawn();
    #[cfg(target_os = "windows")]
    let r = std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let r = std::process::Command::new("xdg-open").arg(path.parent().unwrap_or(path)).spawn();
    if let Err(e) = r {
        log::warn!("cannot reveal {}: {e}", path.display());
    }
}

pub fn show_modals(app: &mut App, ctx: &egui::Context) {
    let enter = ctx.input(|i| i.key_pressed(Key::Enter));
    let escape = ctx.input(|i| i.key_pressed(Key::Escape));

    if let Some((index, mut name)) = app.dialogs.rename.take() {
        let mut keep = true;
        egui::Modal::new(egui::Id::unique("rename-playlist")).show(ctx, |ui| {
            ui.heading("Rename playlist");
            let r = ui.text_edit_singleline(&mut name);
            r.request_focus();
            ui.horizontal(|ui| {
                if ui.button("Rename").clicked() || enter {
                    if let Some(doc) = app.player.lists.get_mut(index)
                        && !name.trim().is_empty()
                    {
                        doc.playlist.name = name.trim().to_owned();
                        app.mark_list_dirty(index);
                    }
                    keep = false;
                }
                if ui.button("Cancel").clicked() || escape {
                    keep = false;
                }
            });
        });
        if keep {
            app.dialogs.rename = Some((index, name));
        }
    }

    if let Some(mut name) = app.dialogs.new_playlist.take() {
        let mut keep = true;
        egui::Modal::new(egui::Id::unique("new-playlist")).show(ctx, |ui| {
            ui.heading("New playlist");
            ui.text_edit_singleline(&mut name).request_focus();
            ui.horizontal(|ui| {
                if ui.button("Create").clicked() || enter {
                    playlist_view::new_playlist(app, &name);
                    keep = false;
                }
                if ui.button("Cancel").clicked() || escape {
                    keep = false;
                }
            });
        });
        if keep {
            app.dialogs.new_playlist = Some(name);
        }
    }

    library_import_modal(app, ctx, escape);
    super::telegram::chooser_modal(app, ctx);

    if let Some(index) = app.dialogs.confirm_clear.take() {
        let mut keep = true;
        egui::Modal::new(egui::Id::unique("confirm-clear")).show(ctx, |ui| {
            let name = app.player.lists.get(index).map(|d| d.playlist.name.clone()).unwrap_or_default();
            ui.heading(format!("Clear “{name}”?"));
            ui.label("All entries are removed from the playlist. Files on disk are not touched.");
            ui.horizontal(|ui| {
                if ui.button(RichText::new("Clear").color(theme::err())).clicked() {
                    app.workers.cancel_scans();
                    app.player.edit_list(index, |p| p.clear());
                    app.selection = Default::default();
                    keep = false;
                }
                if ui.button("Cancel").clicked() || escape {
                    keep = false;
                }
            });
        });
        if keep {
            app.dialogs.confirm_clear = Some(index);
        }
    }

    if let Some(index) = app.dialogs.confirm_delete.take() {
        let mut keep = true;
        egui::Modal::new(egui::Id::unique("confirm-delete")).show(ctx, |ui| {
            let name = app.player.lists.get(index).map(|d| d.playlist.name.clone()).unwrap_or_default();
            ui.heading(format!("Delete playlist “{name}”?"));
            ui.label("The playlist is removed from Nullamp. Audio files are not touched.");
            ui.horizontal(|ui| {
                if ui.button(RichText::new("Delete").color(theme::err())).clicked() {
                    match app.player.remove_list(index) {
                        Some(doc) => {
                            if let Err(e) = app.store.delete_playlist(&doc.key) {
                                app.error(format!("Cannot delete playlist file: {e}"));
                            }
                            app.selection = Default::default();
                            app.mark_settings_dirty();
                        }
                        None => app.warn("Stop playback from this playlist first (and keep at least one playlist)."),
                    }
                    keep = false;
                }
                if ui.button("Cancel").clicked() || escape {
                    keep = false;
                }
            });
        });
        if keep {
            app.dialogs.confirm_delete = Some(index);
        }
    }
}

/// The list of a library's playlists to tick before anything is added, so a big library is not
/// copied whole when only a few playlists are wanted.
fn library_import_modal(app: &mut App, ctx: &egui::Context, escape: bool) {
    let Some(mut choice) = app.dialogs.library_import.take() else { return };
    let mixxx = choice.source == ImportSource::Mixxx;
    let (mut keep, mut import) = (true, false);
    let mut read_data = app.settings.read_dj_data;
    let height = (ctx.content_rect().height() - 230.0).clamp(80.0, 380.0);
    egui::Modal::new(egui::Id::unique("rekordbox-import")).show(ctx, |ui| {
        ui.set_width(380.0);
        ui.heading(if mixxx { "Add from Mixxx" } else { "Add from rekordbox" });
        ui.checkbox(&mut choice.all, format!("Entire {} ({} tracks)", if mixxx { "library" } else { "collection" }, choice.overview.tracks)).on_hover_text("One list with every track of the library");
        ui.separator();
        if choice.overview.items.is_empty() {
            ui.label(RichText::new("The library has no playlists.").weak());
        }
        let all_ids: Vec<String> = choice.overview.items.iter().filter(|i| !i.is_folder).map(|i| i.id.clone()).collect();
        ui.horizontal(|ui| {
            if ui.small_button(if mixxx { "Select all lists" } else { "Select all playlists" }).clicked() {
                choice.selected.extend(all_ids.iter().cloned());
            }
            if ui.small_button("None").clicked() {
                choice.selected.clear();
                choice.all = false;
            }
            ui.label(RichText::new(format!("{} of {} {}", choice.selected.len(), all_ids.len(), if mixxx { "lists" } else { "playlists" })).small().weak());
        });
        egui::ScrollArea::vertical().max_height(height).auto_shrink([false, true]).show(ui, |ui| {
            for index in 0..choice.overview.items.len() {
                let item = choice.overview.items[index].clone();
                ui.horizontal(|ui| {
                    ui.add_space(14.0 * item.depth as f32);
                    if item.is_folder {
                        let below: Vec<String> = choice.overview.below(index).filter(|&i| !choice.overview.items[i].is_folder).map(|i| choice.overview.items[i].id.clone()).collect();
                        let mut ticked = !below.is_empty() && below.iter().all(|id| choice.selected.contains(id));
                        if ui.add_enabled(!below.is_empty(), egui::Checkbox::new(&mut ticked, RichText::new(&item.name).strong())).clicked() {
                            if ticked {
                                choice.selected.extend(below);
                            } else {
                                for id in &below {
                                    choice.selected.remove(id);
                                }
                            }
                        }
                    } else {
                        let mut ticked = choice.selected.contains(&item.id);
                        if ui.checkbox(&mut ticked, &item.name).clicked() {
                            if ticked {
                                choice.selected.insert(item.id.clone());
                            } else {
                                choice.selected.remove(&item.id);
                            }
                        }
                        ui.label(RichText::new(item.tracks.to_string()).small().weak());
                    }
                });
            }
        });
        ui.separator();
        ui.checkbox(&mut read_data, "Also read waveforms, keys and ratings").on_hover_text(format!("Stores what {} knows about each track in the cache in the background, so it does not have to be looked up while playing", if mixxx { "Mixxx" } else { "rekordbox" }));
        ui.horizontal(|ui| {
            let nothing = !choice.all && choice.selected.is_empty();
            if ui.add_enabled(!nothing, egui::Button::new("Add")).clicked() {
                import = true;
                keep = false;
            }
            if ui.button("Cancel").clicked() || escape {
                keep = false;
            }
        });
    });
    if read_data != app.settings.read_dj_data {
        app.settings.read_dj_data = read_data;
        app.mark_settings_dirty();
    }
    if import {
        app.workers.submit(if mixxx { Job::OpenMixxx { all: choice.all, ids: choice.selected, read_data } } else { Job::OpenCollection { all: choice.all, ids: choice.selected, read_data } });
    } else if keep {
        app.dialogs.library_import = Some(choice);
    }
}

/// The settings page takes over the deck instead of floating over the window: in the compact
/// layout the window is only as tall as the deck, and a free window would be cut off.
pub fn show_deck_overlay(app: &mut App, ctx: &egui::Context, deck: egui::Rect) {
    let Some(mut tab) = app.dialogs.settings else {
        app.dialogs.cache.requested = false;
        return;
    };
    let mut close = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape));
    let inner = deck.size() - egui::vec2(20.0, 16.0);
    egui::Area::new(egui::Id::NULL.with("deck-overlay")).order(egui::Order::Foreground).fixed_pos(deck.min).show(ctx, |ui| {
        theme::with_dark(ui, |ui| {
            egui::Frame::new().fill(theme::deck_bg()).inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
                ui.set_min_size(inner);
                ui.set_max_size(inner);
                ui.horizontal(|ui| {
                    for t in SettingsTab::ALL {
                        if widgets::tab(ui, tab == t, t.label()).clicked() {
                            tab = t;
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Close").clicked() {
                            close = true;
                        }
                        if tab == SettingsTab::Shortcuts {
                            ui.label(RichText::new("Hardware media keys work while Nullamp is in the background.").small().color(theme::text_dim()));
                        }
                    });
                });
                ui.separator();
                match tab {
                    SettingsTab::General => general_body(app, ui),
                    SettingsTab::Audio => super::deck::output_page(app, ui),
                    SettingsTab::Shortcuts => shortcuts_body(ui),
                    SettingsTab::About => about_body(app, ui),
                    SettingsTab::Logs => logs_body(ui),
                    SettingsTab::Updates => updates_body(app, ui),
                    SettingsTab::Cache => cache_body(app, ui),
                    SettingsTab::Rekordbox => rekordbox_body(app, ui),
                    SettingsTab::Web => web_body(app, ui),
                    SettingsTab::Telegram => super::telegram::body(app, ui),
                }
            });
        });
    });
    if tab != SettingsTab::Cache {
        app.dialogs.cache.requested = false;
    }
    app.dialogs.settings = if close { None } else { Some(tab) };
}

/// Appearance, as a pick-one list like the audio outputs. The deck stays dark in every theme, so
/// the rest of the window shows the change.
fn general_body(app: &mut App, ui: &mut egui::Ui) {
    let current = theme::Appearance::from_setting(&app.settings.appearance);
    ui.label(RichText::new("Appearance").small().weak());
    ui.vertical(|ui| {
        ui.set_width(260.0);
        let rows = [(theme::Appearance::Dark, "Dark"), (theme::Appearance::Light, "Light"), (theme::Appearance::System, "System (follow macOS)")];
        for (appearance, label) in rows {
            if super::deck::choice_row(ui, label, true, current == appearance, |p, rect, color| widgets::paint_appearance_icon(p, rect, appearance, color)) && current != appearance {
                app.settings.appearance = appearance.setting().into();
                theme::set_appearance(ui.ctx(), appearance);
                app.mark_settings_dirty();
            }
        }
    });
}

/// What Nullamp reads from websites on the user's behalf, when the user asks.
fn web_body(app: &mut App, ui: &mut egui::Ui) {
    ui.label(RichText::new("1001Tracklists").small().weak());
    ui.horizontal(|ui| {
        ui.label("guid cookie:");
        let field = ui.add(egui::TextEdit::singleline(&mut app.settings.tracklists_guid).password(true).hint_text("cookie value").desired_width(240.0));
        if field.changed() {
            app.settings.tracklists_guid = app.settings.tracklists_guid.trim().to_owned();
            app.mark_settings_dirty();
        }
    });
    ui.label(RichText::new("For Import > Search 1001Tracklists in the setlist. 1001Tracklists answers tracklist pages only to a browser it knows: open the site in your browser and copy the value of its \"guid\" cookie here (Web Inspector, Storage, Cookies). It is sent to 1001Tracklists only.").small().weak());
}

/// What Nullamp reads from rekordbox: the analysis behind the waveform, and the desktop library.
/// Kept to a few lines because the page only has the height of the deck.
fn rekordbox_body(app: &mut App, ui: &mut egui::Ui) {
    if ui.checkbox(&mut app.settings.rekordbox_waveform, "Use rekordbox's and Mixxx's waveform analysis").on_hover_text("For tracks on rekordbox USB sticks, in the rekordbox desktop library and in the Mixxx library. Takes effect when the next track starts").changed() {
        app.mark_settings_dirty();
    }
    ui.horizontal(|ui| {
        ui.label("master.db key:");
        let field = ui.add(egui::TextEdit::singleline(&mut app.settings.rekordbox_key).password(true).hint_text("64 hex characters").desired_width(240.0));
        if field.changed() {
            app.workers.set_rekordbox_key(&app.settings.rekordbox_key);
            app.mark_settings_dirty();
        }
        if ui.small_button("Copy key command").on_hover_text("Installs pyrekordbox in a private virtualenv and prints the key").clicked() {
            ui.ctx().copy_text(REKORDBOX_KEY_COMMAND.to_owned());
        }
        ui.hyperlink_to(RichText::new("pyrekordbox docs").small(), "https://pyrekordbox.readthedocs.io");
    });
    if app.dialogs.python == PythonState::Unknown {
        check_python(app);
    }
    ui.horizontal(|ui| {
        match &app.dialogs.python {
            PythonState::Unknown | PythonState::Checking => ui.label(RichText::new("Looking for Python 3…").small().weak()),
            PythonState::Found(version) => ui.label(RichText::new(format!("Python {version} found, the key command can run")).small().color(theme::ok())),
            PythonState::Missing => ui.label(RichText::new(format!("No Python 3 found, the key command needs one ({})", INSTALL_PYTHON)).small().color(theme::warn())),
        };
        if !matches!(app.dialogs.python, PythonState::Unknown | PythonState::Checking) && ui.small_button("Check again").clicked() {
            app.dialogs.python = PythonState::Unknown;
        }
    });
    ui.label(RichText::new(format!("The key opens your rekordbox collection from the List menu and fills in key and BPM for your files. rekordbox 7 uses the same key on every computer: with rekordbox closed, copy the command, run it in {SHELL} and paste the 64-character result here.")).small().weak());
}

/// A one-time, copy-paste recipe that installs pyrekordbox into a private virtualenv and prints the
/// rekordbox desktop key. Kept out of the UI code so it stays easy to update. The database class is
/// looked up on the package itself because its module and name differ between pyrekordbox releases
/// (older ones, which an old system Python gets, only have `Rekordbox6Database`).
#[cfg(not(windows))]
const REKORDBOX_KEY_COMMAND: &str = "python3 -m venv ~/.rekordbox-key && ~/.rekordbox-key/bin/pip install -q pyrekordbox && ~/.rekordbox-key/bin/python -c \"import warnings;warnings.filterwarnings('ignore');import pyrekordbox as p;print((getattr(p,'MasterDatabase',None) or p.Rekordbox6Database)().engine.url.password)\"";

/// The same recipe for PowerShell, where the virtualenv's tools live in `Scripts`.
#[cfg(windows)]
const REKORDBOX_KEY_COMMAND: &str = "python -m venv $HOME\\.rekordbox-key; & $HOME\\.rekordbox-key\\Scripts\\pip install -q pyrekordbox; & $HOME\\.rekordbox-key\\Scripts\\python -c \"import warnings;warnings.filterwarnings('ignore');import pyrekordbox as p;print((getattr(p,'MasterDatabase',None) or p.Rekordbox6Database)().engine.url.password)\"";

#[cfg(target_os = "macos")]
const INSTALL_PYTHON: &str = "run xcode-select --install in Terminal or install it from python.org";
#[cfg(windows)]
const INSTALL_PYTHON: &str = "install it from python.org";
#[cfg(not(any(target_os = "macos", windows)))]
const INSTALL_PYTHON: &str = "install python3 with your package manager";

#[cfg(windows)]
const SHELL: &str = "PowerShell";
#[cfg(not(windows))]
const SHELL: &str = "Terminal";

/// The shortcut list in as many columns as the deck is wide, so it fits its height.
fn shortcuts_body(ui: &mut egui::Ui) {
    let alt = if cfg!(target_os = "macos") { "⌥" } else { "Alt+" };
    let rows: Vec<(String, &str)> = vec![
        ("Space / C".into(), "Play / pause"),
        ("X".into(), "Play"),
        ("V".into(), "Stop"),
        ("Z / B".into(), "Prev / next track"),
        ("← / →".into(), "Seek 5 s"),
        ("⇧← / ⇧→".into(), "Prev / next setlist entry"),
        ("+ / − / M".into(), "Volume / mute"),
        ("S / R".into(), "Shuffle / repeat"),
        ("D".into(), "Dim everything but the display and artwork"),
        ("↑ ↓ Home End".into(), "Select (⇧ extends)"),
        ("Enter".into(), "Play selected"),
        ("Del / Backspace".into(), "Remove selected"),
        (format!("{alt}↑ / {alt}↓"), "Move selected"),
        (widgets::cmd_key('A', false), "Select all"),
        (format!("{} / {}", widgets::cmd_key('O', false), widgets::cmd_key('O', true)), "Add files / folder"),
        (format!("{} / {} / {}", widgets::cmd_key('I', false), widgets::cmd_key('L', false), widgets::cmd_key('P', false)), "Inspector / setlist / list"),
        (widgets::cmd_key('J', false), "Show playing track"),
        (widgets::cmd_key('F', false), "Filter the playlist"),
        ("F1".into(), "This help"),
    ];
    const COLUMNS: usize = 3;
    let per_column = rows.len().div_ceil(COLUMNS);
    egui::ScrollArea::both().id_salt("shortcuts-scroll").auto_shrink([false, false]).show(ui, |ui| {
        ui.horizontal_top(|ui| {
            for (n, chunk) in rows.chunks(per_column).enumerate() {
                if n > 0 {
                    ui.add_space(10.0);
                }
                egui::Grid::new(("shortcuts", n)).num_columns(2).spacing([8.0, 2.0]).show(ui, |ui| {
                    for (k, v) in chunk {
                        ui.label(RichText::new(k).size(12.0).strong().color(theme::key()));
                        ui.label(RichText::new(*v).size(12.0));
                        ui.end_row();
                    }
                });
            }
        });
    });
}

/// Product info.
fn about_body(app: &mut App, ui: &mut egui::Ui) {
    let icon = app.dialogs.about_icon.get_or_insert_with(|| {
        let data = crate::app_icon();
        let image = egui::ColorImage::from_rgba_unmultiplied([data.width as usize, data.height as usize], &data.rgba);
        ui.ctx().load_texture("about-icon", image, egui::TextureOptions::LINEAR)
    });
    ui.horizontal_top(|ui| {
        ui.add(egui::Image::new(&*icon).fit_to_exact_size(egui::vec2(72.0, 72.0)));
        ui.vertical(|ui| {
            ui.heading(format!("Nullamp {}", env!("CARGO_PKG_VERSION")));
            ui.label("A fast, retro-inspired music and DJ mix player.");
            ui.label(RichText::new(format!("License: {}", env!("CARGO_PKG_LICENSE"))).small().color(theme::text_dim()));
        });
    });
}

/// The tail of the log, newest line at the bottom.
fn logs_body(ui: &mut egui::Ui) {
    let height = ui.available_height();
    let lines = crate::logbuf::tail();
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("Log (last {} lines)", lines.len())).small().color(theme::text_dim()));
        if ui.small_button("Copy").clicked() {
            ui.ctx().copy_text(lines.join("\n"));
        }
    });
    egui::Frame::new().fill(theme::bg()).stroke(egui::Stroke::new(1.0, theme::border())).inner_margin(egui::Margin::same(4)).show(ui, |ui| {
        let size = egui::vec2(ui.available_width(), (height - 30.0).max(40.0));
        ui.set_min_size(size);
        egui::ScrollArea::both().id_salt("about-log").max_height(size.y).auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
            if lines.is_empty() {
                ui.label(RichText::new("Nothing logged yet.").small().color(theme::text_faint()));
            }
            for l in &lines {
                ui.add(egui::Label::new(RichText::new(l).font(theme::mono(11.0)).color(theme::text())).extend());
            }
        });
    });
}

/// The running version, a button that asks whether a newer one exists, and the answer.
fn updates_body(app: &mut App, ui: &mut egui::Ui) {
    use nullamp_update::Status;
    ui.label(RichText::new(format!("You are running Nullamp {}.", env!("CARGO_PKG_VERSION"))));
    ui.add_space(6.0);
    let checking = matches!(app.dialogs.update, UpdateState::Checking);
    if ui.add_enabled(!checking, egui::Button::new("Check for updates")).clicked() {
        check_for_updates(app);
    }
    ui.add_space(6.0);
    let dim = theme::text_dim();
    match &app.dialogs.update {
        UpdateState::Idle => {
            ui.label(RichText::new("Nullamp only contacts the update address when you press the button.").small().color(dim));
        }
        UpdateState::Checking => {
            ui.label("Checking for updates...");
        }
        UpdateState::Done(Ok(Status::UpToDate { .. })) => {
            ui.label("You have the latest version.");
        }
        UpdateState::Done(Ok(Status::Available(release))) => {
            ui.label(format!("Version {} is available.", release.version));
            if let Some(page) = &release.page {
                ui.hyperlink_to("Open the download page", page).on_hover_text(page);
            }
        }
        UpdateState::Done(Err(e)) => {
            ui.label(format!("Couldn't check for updates: {e}."));
        }
    }
}

fn files(n: usize) -> String {
    format!("{n} {}", if n == 1 { "file" } else { "files" })
}

/// What the cache holds, with a button to clear each kind and all of it.
fn cache_body(app: &mut App, ui: &mut egui::Ui) {
    if !app.dialogs.cache.requested {
        scan_cache(app, Vec::new());
    }
    let dim = theme::text_dim();
    let busy = app.dialogs.cache.busy;
    let mut clear: Option<Vec<Kind>> = None;
    match app.dialogs.cache.usage.clone() {
        None => {
            ui.label(RichText::new("Reading the cache...").small().color(dim));
        }
        Some(usage) => {
            let total = usage.iter().fold(Usage::default(), |t, (_, u)| Usage { entries: t.entries + u.entries, bytes: t.bytes + u.bytes });
            ui.horizontal(|ui| {
                ui.label(format!("{} in {}", cache::format_bytes(total.bytes), files(total.entries)));
                if ui.small_button("Show in Finder").clicked() {
                    reveal(&app.store.paths.cache);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add_enabled(!busy && total.entries > 0, egui::Button::new("Clear all")).on_hover_text("Everything here is made again when needed").clicked() {
                        clear = Some(Kind::ALL.to_vec());
                    }
                });
            });
            ui.add_space(4.0);
            egui::ScrollArea::vertical().id_salt("cache-scroll").auto_shrink([false, false]).show(ui, |ui| {
                ui.spacing_mut().interact_size.y = 16.0;
                ui.spacing_mut().button_padding.y = 1.0;
                egui::Grid::new("cache-usage").num_columns(4).spacing([14.0, 2.0]).show(ui, |ui| {
                    // Leftovers only matter to someone who ran older versions.
                    for (kind, u) in usage.iter().filter(|(k, u)| *k != Kind::Leftovers || u.entries > 0) {
                        ui.label(RichText::new(kind.label()).size(12.0)).on_hover_text(kind.about());
                        ui.label(RichText::new(files(u.entries)).size(12.0).color(dim));
                        ui.label(RichText::new(cache::format_bytes(u.bytes)).size(12.0).color(dim));
                        if ui.add_enabled(!busy && u.entries > 0, egui::Button::new(RichText::new("Clear").size(11.0)).small()).on_hover_text(kind.about()).clicked() {
                            clear = Some(vec![*kind]);
                        }
                        ui.end_row();
                    }
                });
            });
        }
    }
    if let Some(kinds) = clear {
        scan_cache(app, kinds);
    }
}
