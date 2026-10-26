//! Native file dialogs (run off the UI thread) and in-app modal dialogs.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crossbeam_channel::{Receiver, Sender};
use egui::{Key, RichText};

use nullamp_core::EntryId;

use super::{App, list_index, playlist_view, setlist_view, theme, widgets};
use crate::cache::{self, Kind, Usage};
use crate::rekordbox::Overview;
use crate::workers::{AUDIO_EXTENSIONS, Job};

pub enum DialogResult {
    AddFiles { list: String, paths: Vec<PathBuf> },
    /// The file picked with Browse in the Change file dialog.
    ChangeFileBrowsed(PathBuf),
    /// The folder picked with Search folder in the Change file dialog.
    ChangeFileFolder(PathBuf),
    ImportPlaylist { list: String, path: PathBuf },
    ExportPlaylist { list: String, path: PathBuf },
    ImportSetlist(PathBuf),
    ExportSetlist { format: setlist_view::ExportFormat, path: PathBuf },
    ExportData(PathBuf),
    ImportData(PathBuf),
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

    /// The tabs this build has: without the online extras there is no Web or Telegram tab, without the update check no Updates tab.
    pub(super) fn shown() -> impl Iterator<Item = SettingsTab> {
        Self::ALL.into_iter().filter(|t| match t {
            SettingsTab::Web | SettingsTab::Telegram => crate::ONLINE_EXTRAS,
            SettingsTab::Updates => crate::UPDATE_CHECK,
            _ => true,
        })
    }

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
    /// The in-window file browser that stands in for the native dialog where there is none.
    pub browser: Option<super::file_browser::Browser>,
    pub change_file: Option<ChangeFile>,
    pub spotify: Option<super::spotify::Import>,
}

impl Dialogs {
    pub fn new() -> Self {
        let (tx, results) = crossbeam_channel::unbounded();
        Dialogs { tx, results, rename: None, new_playlist: None, confirm_clear: None, confirm_delete: None, settings: None, about_icon: None, update: UpdateState::Idle, cache: CacheState::default(), python: PythonState::Unknown, library_import: None, browser: None, change_file: None, spotify: None }
    }
}

/// Android has no native file dialog; see `android_dialogs`.
#[cfg(target_os = "android")]
use super::android_dialogs as rfd;

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

/// The "Change file" dialog of a playlist row: the track's file now, and the text of the file it should play instead.
pub struct ChangeFile {
    list: String,
    entry: EntryId,
    old: PathBuf,
    /// The new path as typed, pasted or browsed.
    text: String,
    error: Option<String>,
    search: Searched,
}

/// Where the dialog's search for the missing file stands.
enum Searched {
    Idle,
    /// Looking in this folder, for the request with this id.
    Running { request: u64, root: PathBuf },
    Done { root: PathBuf, search: crate::locate::Search, secs: f64 },
}

/// "Change file…" of a playlist row: opens the dialog with the track's current path to edit.
pub fn change_file(app: &mut App, list: usize, entry: EntryId) {
    let Some(old) = app.player.lists.get(list).and_then(|d| d.playlist.get(entry)).map(|e| e.path.clone()) else { return };
    let key = app.player.lists[list].key.clone();
    app.dialogs.change_file = Some(ChangeFile { list: key, entry, text: old.display().to_string(), old, error: None, search: Searched::Idle });
}

/// Browse for the new file, starting in the folder of the one typed, else of the old one.
fn browse_change_file(app: &App) {
    let Some(c) = app.dialogs.change_file.as_ref() else { return };
    let dir = [parse_path(&c.text), c.old.clone()].iter().find_map(|p| p.parent().filter(|d| d.is_dir()).map(Path::to_path_buf)).or_else(|| start_dir(app));
    spawn(app, move || async move {
        let mut d = rfd::AsyncFileDialog::new().set_title("Choose file").add_filter("Audio", AUDIO_EXTENSIONS);
        if let Some(dir) = dir {
            d = d.set_directory(dir);
        }
        let file = d.pick_file().await?;
        Some(DialogResult::ChangeFileBrowsed(file.path().to_path_buf()))
    });
}

/// Choose the folder to look for the missing file in, starting at the last one searched, else the nearest
/// folder of the old path that exists.
fn browse_search_folder(app: &App) {
    let Some(c) = app.dialogs.change_file.as_ref() else { return };
    let searched = match &c.search {
        Searched::Running { root, .. } | Searched::Done { root, .. } => Some(root.clone()),
        Searched::Idle => None,
    };
    let dir = searched.or_else(|| c.old.ancestors().skip(1).find(|d| d.is_dir()).map(Path::to_path_buf)).or_else(|| start_dir(app));
    spawn(app, move || async move {
        let mut d = rfd::AsyncFileDialog::new().set_title("Search folder");
        if let Some(dir) = dir {
            d = d.set_directory(dir);
        }
        let folder = d.pick_folder().await?;
        Some(DialogResult::ChangeFileFolder(folder.path().to_path_buf()))
    });
}

/// Start looking for the missing file in `root`.
fn start_search(app: &mut App, root: PathBuf) {
    let request = app.workers.request_id();
    let Some(c) = app.dialogs.change_file.as_mut() else { return };
    app.workers.locate(request, root.clone(), c.old.clone());
    c.search = Searched::Running { request, root };
}

/// What the search found. One file of the very name fills the field, since it is nearly always the one.
pub fn on_located(app: &mut App, request: u64, root: PathBuf, search: crate::locate::Search, secs: f64) {
    let Some(c) = app.dialogs.change_file.as_mut() else { return };
    if !matches!(&c.search, Searched::Running { request: r, .. } if *r == request) {
        return;
    }
    use crate::locate::Match;
    let exact = search.found.iter().filter(|f| f.kind == Match::Name).count();
    if let (1, Some(best)) = (exact, search.found.first()) {
        (c.text, c.error) = (best.path.display().to_string(), None);
    }
    c.search = Searched::Done { root, search, secs };
}

/// The search's state and its results, as rows to click.
fn search_results(ui: &mut egui::Ui, c: &mut ChangeFile) {
    use crate::locate::Match;
    match &c.search {
        Searched::Idle => {}
        Searched::Running { root, .. } => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(format!("Looking in {}…", root.display())).small().color(theme::text_faint()));
            });
        }
        Searched::Done { root, search, secs } => {
            let looked = format!("{} looked at in {secs:.1} s", super::plural(search.scanned, "audio file"));
            let skipped = if search.skipped > 0 { format!(", {} could not be read", super::plural(search.skipped, "folder")) } else { String::new() };
            let summary = match search.found.len() {
                0 => format!("Nothing like it in {} ({looked}{skipped})", root.display()),
                n if search.more => format!("The {n} best matches in {} ({looked}{skipped})", root.display()),
                1 => format!("1 match in {} ({looked}{skipped})", root.display()),
                n => format!("{n} matches in {} ({looked}{skipped})", root.display()),
            };
            ui.label(RichText::new(summary).small().color(theme::text_faint()));
            let mut chosen = None;
            egui::ScrollArea::vertical().max_height(180.0).auto_shrink([false, true]).show(ui, |ui| {
                for f in &search.found {
                    let shown = f.path.strip_prefix(root).unwrap_or(&f.path).display().to_string();
                    let note = match f.kind {
                        Match::Name => "",
                        Match::Format => "  (other format)",
                        Match::Similar => "  (similar name)",
                    };
                    let selected = parse_path(&c.text) == f.path;
                    if ui.selectable_label(selected, format!("{shown}{note}")).on_hover_text(f.path.display().to_string()).clicked() {
                        chosen = Some(f.path.display().to_string());
                    }
                }
            });
            if let Some(text) = chosen {
                (c.text, c.error) = (text, None);
            }
        }
    }
}

/// A path as people paste it: with quotes around it, `\ ` for the spaces (a terminal's drag and drop), or a
/// leading `~`.
fn parse_path(text: &str) -> PathBuf {
    let text = text.trim().trim_matches(['"', '\'']).trim();
    let text = text.replace("\\ ", " ");
    match (text.strip_prefix("~/"), directories::UserDirs::new()) {
        (Some(rest), Some(dirs)) => dirs.home_dir().join(rest),
        _ => PathBuf::from(text),
    }
}

/// Point the track at the typed path, when it names an audio file; else say what is wrong and keep the dialog.
fn apply_change_file(app: &mut App) -> bool {
    let Some(c) = app.dialogs.change_file.as_mut() else { return true };
    let path = parse_path(&c.text);
    c.error = if !path.is_file() {
        Some("There is no file at that path".into())
    } else if !crate::workers::is_audio_file(&path) {
        Some("That is not an audio file Nullamp can play".into())
    } else {
        None
    };
    if c.error.is_some() {
        return false;
    }
    let (list, entry) = (c.list.clone(), c.entry);
    app.dialogs.change_file = None;
    if let Some(i) = list_index(app, &list) {
        app.settings.last_dir = path.parent().map(Path::to_path_buf);
        app.mark_settings_dirty();
        app.change_entry_file(i, entry, path);
    }
    true
}

fn change_file_modal(app: &mut App, ctx: &egui::Context, enter: bool, escape: bool) {
    let Some(mut c) = app.dialogs.change_file.take() else { return };
    let (mut keep, mut apply, mut browse, mut search) = (true, false, false, false);
    egui::Modal::new(egui::Id::unique("change-file")).show(ctx, |ui| {
        ui.set_width(widgets::modal_width(ctx, 560.0));
        ui.heading("Change file");
        ui.horizontal(|ui| {
            let field = ui.add(egui::TextEdit::singleline(&mut c.text).desired_width(ui.available_width() - 90.0).font(egui::TextStyle::Monospace));
            if field.changed() {
                c.error = None;
            }
            if !field.has_focus() && !browse {
                field.request_focus();
            }
            if ui.button("Browse…").clicked() {
                browse = true;
            }
        });
        if ui.button("Search folder…").on_hover_text("Look for a file of this name in a folder and the folders below it").clicked() {
            search = true;
        }
        search_results(ui, &mut c);
        if let Some(error) = &c.error {
            ui.label(RichText::new(error).small().color(theme::err()));
        }
        ui.horizontal(|ui| {
            let ready = !c.text.trim().is_empty() && parse_path(&c.text) != c.old;
            if ui.add_enabled(ready, egui::Button::new("Change")).clicked() || (ready && enter) {
                apply = true;
            }
            if ui.button("Cancel").clicked() || escape {
                keep = false;
            }
        });
    });
    app.dialogs.change_file = Some(c);
    if browse {
        browse_change_file(app);
    }
    if search {
        browse_search_folder(app);
    }
    if (apply && apply_change_file(app)) || !keep {
        app.dialogs.change_file = None;
        app.workers.cancel_locate();
    }
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

pub fn export_data(app: &mut App) {
    spawn(app, move || async move {
        let f = rfd::AsyncFileDialog::new().set_title("Export data for another device").set_file_name("Nullamp data.zip").add_filter("Nullamp data", &["zip"]).save_file().await?;
        Some(DialogResult::ExportData(f.path().to_path_buf()))
    });
}

pub fn import_data(app: &mut App) {
    spawn(app, move || async move {
        let f = rfd::AsyncFileDialog::new().set_title("Import data from another device").add_filter("Nullamp data", &["zip"]).pick_file().await?;
        Some(DialogResult::ImportData(f.path().to_path_buf()))
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

pub(super) fn sanitize(name: &str) -> String {
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
        DialogResult::ChangeFileBrowsed(path) => {
            if let Some(c) = app.dialogs.change_file.as_mut() {
                (c.text, c.error) = (path.display().to_string(), None);
            }
        }
        DialogResult::ChangeFileFolder(root) => {
            remember_dir(app, &root);
            start_search(app, root);
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
        DialogResult::ExportData(path) => app.workers.submit(Job::ExportData { path }),
        DialogResult::ImportData(path) => app.workers.submit(Job::ImportData { path }),
        DialogResult::Artwork(path) => super::inspector::add_artwork_file(app, path),
        DialogResult::TelegramDir(dir) => {
            app.settings.telegram_dir = Some(dir);
            app.mark_settings_dirty();
        }
    }
}

/// Show a file selected in the file manager through the `org.freedesktop.FileManager1` D-Bus service that
/// Nautilus, Dolphin, Nemo, Thunar and others implement; a desktop without it gets the folder opened.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn reveal_on_linux(path: &Path) -> std::io::Result<()> {
    let folder = path.parent().unwrap_or(path).to_path_buf();
    let uri = nullamp_sysmedia::file_url(path);
    std::thread::spawn(move || {
        let shown = uri.is_some_and(|uri| {
            std::process::Command::new("dbus-send")
                .args(["--session", "--print-reply", "--dest=org.freedesktop.FileManager1", "/org/freedesktop/FileManager1", "org.freedesktop.FileManager1.ShowItems"])
                .arg(format!("array:string:{uri}"))
                .arg("string:")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        });
        if !shown && let Err(e) = std::process::Command::new("xdg-open").arg(&folder).spawn() {
            log::warn!("cannot reveal in {}: {e}", folder.display());
        }
    });
    Ok(())
}

/// Open a text file in the platform's default editor.
pub fn open_in_editor(path: &Path) -> std::io::Result<std::process::Child> {
    #[cfg(target_os = "macos")]
    return std::process::Command::new("open").arg("-t").arg(path).spawn();
    #[cfg(target_os = "windows")]
    return std::process::Command::new("cmd").args(["/C", "start", ""]).arg(path).spawn();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    return std::process::Command::new("xdg-open").arg(path).spawn();
}

/// Reveal a file in the platform's file manager.
pub fn reveal(path: &Path) {
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg("-R").arg(path).spawn();
    #[cfg(target_os = "windows")]
    let r = std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let r = reveal_on_linux(path);
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
    change_file_modal(app, ctx, enter, escape);
    super::spotify::modal(app, ctx, enter, escape);

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
        ui.set_width(widgets::modal_width(ctx, 380.0));
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

/// Narrower than this, the settings tabs wrap onto more rows (all of them take about 640 points).
const SETTINGS_TABS_WIDTH: f32 = 680.0;

/// The settings page takes over the deck instead of floating over the window: in the compact
/// layout the window is only as tall as the deck, and a free window would be cut off.
pub fn show_deck_overlay(app: &mut App, ctx: &egui::Context, deck: egui::Rect) {
    let Some(mut tab) = app.dialogs.settings else {
        app.dialogs.cache.requested = false;
        return;
    };
    let mut close = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape));
    // Below the system bars, as the deck itself is.
    let (bars_top, _) = crate::system_bar_insets(ctx.pixels_per_point());
    let inner = deck.size() - egui::vec2(20.0, 16.0 + bars_top);
    egui::Area::new(egui::Id::NULL.with("deck-overlay")).order(egui::Order::Foreground).fixed_pos(deck.min).show(ctx, |ui| {
        theme::with_dark(ui, |ui| {
            egui::Frame::new().fill(theme::deck_bg()).inner_margin(egui::Margin { left: 10, right: 10, top: 8 + bars_top as i8, bottom: 8 }).show(ui, |ui| {
                ui.set_min_size(inner);
                ui.set_max_size(inner);
                if ui.available_width() < SETTINGS_TABS_WIDTH {
                    // A phone: the tabs wrap onto more rows, and Close ends the last.
                    ui.horizontal_wrapped(|ui| {
                        for t in SettingsTab::shown() {
                            if widgets::tab(ui, tab == t, t.label()).clicked() {
                                tab = t;
                            }
                        }
                        if ui.small_button("Close").clicked() {
                            close = true;
                        }
                    });
                } else {
                    ui.horizontal(|ui| {
                        for t in SettingsTab::shown() {
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
                }
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

/// The in-window file browser, over everything else while it is open.
pub fn show_browser(app: &mut App, ctx: &egui::Context) {
    if let Some(mut browser) = app.dialogs.browser.take()
        && browser.show(ctx)
    {
        app.dialogs.browser = Some(browser);
    }
}

/// Appearance, as a pick-one list like the audio outputs. The deck stays dark in every theme, so
/// the rest of the window shows the change.
fn general_body(app: &mut App, ui: &mut egui::Ui) {
    if ui.checkbox(&mut app.settings.show_status_bar, "Show the status bar").on_hover_text("The strip along the bottom with the track count and messages").changed() {
        app.mark_settings_dirty();
    }
    if cfg!(target_os = "android") {
        ui.add_space(4.0);
        if crate::all_files_access() {
            ui.label(RichText::new("Access to all files is allowed: Nullamp can list your music and save tags to it.").small().weak());
        } else {
            ui.label(RichText::new("Nullamp needs access to all files to list your music and to save tags to it.").small().color(theme::LCD_AMBER));
            if ui.button("Open the permission page").clicked() {
                crate::request_all_files_access();
            }
        }
    }
    ui.add_space(4.0);
    let current = theme::Appearance::from_setting(&app.settings.appearance);
    let rows = [(theme::Appearance::Dark, "Dark"), (theme::Appearance::Light, "Light"), (theme::Appearance::System, if cfg!(target_os = "macos") { "System (follow macOS)" } else { "System" })];
    ui.horizontal(|ui| {
        ui.label("Appearance:");
        let shown = rows.iter().find(|(a, _)| *a == current).map_or("Dark", |(_, label)| *label);
        egui::ComboBox::from_id_salt("appearance").selected_text(shown).show_ui(ui, |ui| {
            for (appearance, label) in rows {
                if ui.selectable_label(current == appearance, label).clicked() && current != appearance {
                    app.settings.appearance = appearance.setting().into();
                    theme::set_appearance(ui.ctx(), appearance);
                    app.mark_settings_dirty();
                }
            }
        });
    });
    ui.add_space(8.0);
    ui.label(RichText::new("Another device").small().weak());
    ui.horizontal(|ui| {
        if ui.button("Export data…").on_hover_text("Your setlists and what rekordbox and Mixxx know about your files, in one file").clicked() {
            export_data(app);
        }
        if ui.button("Import data…").on_hover_text("Bring in a file made by Nullamp on another device").clicked() {
            import_data(app);
        }
    });
    ui.label(RichText::new("For a copy of Nullamp that has no rekordbox or Mixxx, such as on a phone. Waveforms, tempo and covers are made again there.").small().color(theme::text_dim()));
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

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use nullamp_core::EntryStatus;

    use super::*;
    use crate::ui_snapshots::{seeded_store, settle};

    #[test]
    fn settings_tabs_follow_the_build_features() {
        let tabs: Vec<_> = SettingsTab::shown().collect();
        assert_eq!(tabs.contains(&SettingsTab::Web), cfg!(feature = "online-extras"));
        assert_eq!(tabs.contains(&SettingsTab::Telegram), cfg!(feature = "online-extras"));
        assert_eq!(tabs.contains(&SettingsTab::Updates), cfg!(feature = "update-check"));
        assert!(tabs.contains(&SettingsTab::General) && tabs.contains(&SettingsTab::About));
    }

    #[test]
    fn pasted_paths_are_cleaned_up() {
        assert_eq!(parse_path("  /Music/a b.mp3\n"), PathBuf::from("/Music/a b.mp3"));
        assert_eq!(parse_path("\"/Music/a b.mp3\""), PathBuf::from("/Music/a b.mp3"));
        assert_eq!(parse_path("'/Music/a b.mp3'"), PathBuf::from("/Music/a b.mp3"));
        assert_eq!(parse_path("/Music/a\\ b.mp3"), PathBuf::from("/Music/a b.mp3"));
        if let Some(dirs) = directories::UserDirs::new() {
            assert_eq!(parse_path("~/Music/a.mp3"), dirs.home_dir().join("Music/a.mp3"));
        }
    }

    /// The dialog starts with the track's path; a path that is no file or no audio is explained and keeps the
    /// dialog open, and an audio file is taken, remembered as the folder to browse from.
    #[test]
    fn the_dialog_takes_a_typed_path_that_names_an_audio_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = seeded_store(dir.path(), "dark");
        let mut h = Harness::builder().with_size(egui::vec2(1180.0, 740.0)).build_eframe(|cc| App::new(cc, store));
        settle(&mut h, 300);
        let app = h.state_mut();
        let entries = app.player.lists[0].playlist.entries();
        let (id, old, new) = (entries[6].id, entries[6].path.clone(), entries[1].path.clone());

        change_file(app, 0, id);
        assert_eq!(app.dialogs.change_file.as_ref().unwrap().text, old.display().to_string());

        app.dialogs.change_file.as_mut().unwrap().text = format!("{}.nope", new.display());
        assert!(!apply_change_file(app));
        assert_eq!(app.dialogs.change_file.as_ref().unwrap().error.as_deref(), Some("There is no file at that path"));
        let notes = dir.path().join("notes.txt");
        std::fs::write(&notes, "not music").unwrap();
        app.dialogs.change_file.as_mut().unwrap().text = notes.display().to_string();
        assert!(!apply_change_file(app));
        assert!(app.dialogs.change_file.as_ref().unwrap().error.as_deref().is_some_and(|e| e.contains("not an audio file")));
        assert_eq!(app.player.lists[0].playlist.get(id).unwrap().path, old);

        // Browsing fills the field and clears the complaint.
        on_dialog_result(app, DialogResult::ChangeFileBrowsed(new.clone()));
        assert!(app.dialogs.change_file.as_ref().unwrap().error.is_none());
        assert!(apply_change_file(app));
        assert!(app.dialogs.change_file.is_none());
        let track = app.player.lists[0].playlist.get(id).unwrap();
        assert_eq!((&track.path, &track.status), (&new, &EntryStatus::Unknown));
        assert_eq!(app.settings.last_dir.as_deref(), new.parent());
    }

    /// Searching a folder finds a copy of the file by its name, fills the field with it, and a search that
    /// the dialog has closed on is dropped.
    #[test]
    fn searching_a_folder_fills_the_field_with_the_one_file_of_that_name() {
        let dir = tempfile::tempdir().unwrap();
        let store = seeded_store(dir.path(), "dark");
        let mut h = Harness::builder().with_size(egui::vec2(1180.0, 740.0)).build_eframe(|cc| App::new(cc, store));
        settle(&mut h, 300);
        let app = h.state_mut();
        let entries = app.player.lists[0].playlist.entries();
        let (id, old) = (entries[6].id, entries[6].path.clone());
        let moved = dir.path().join("music/moved").join(old.file_name().unwrap());
        std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
        std::fs::write(&moved, b"x").unwrap();

        change_file(app, 0, id);
        on_dialog_result(app, DialogResult::ChangeFileFolder(dir.path().join("music")));
        assert!(matches!(app.dialogs.change_file.as_ref().unwrap().search, Searched::Running { .. }));
        for _ in 0..40 {
            settle(&mut h, 50);
            if matches!(h.state_mut().dialogs.change_file.as_ref().unwrap().search, Searched::Done { .. }) {
                break;
            }
        }
        let c = h.state_mut().dialogs.change_file.as_ref().unwrap();
        assert!(matches!(c.search, Searched::Done { .. }), "the search answers");
        assert_eq!(c.text, moved.display().to_string());
    }
}
