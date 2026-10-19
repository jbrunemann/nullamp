//! Native file dialogs (run off the UI thread) and in-app modal dialogs.

use std::path::{Path, PathBuf};

use crossbeam_channel::{Receiver, Sender};
use egui::{Key, RichText};

use super::{App, list_index, playlist_view, setlist_view, theme};
use crate::workers::{AUDIO_EXTENSIONS, Job};

pub enum DialogResult {
    AddFiles { list: String, paths: Vec<PathBuf> },
    ImportPlaylist { list: String, path: PathBuf },
    ExportPlaylist { list: String, path: PathBuf },
    ImportSetlist(PathBuf),
    ExportSetlist { format: setlist_view::ExportFormat, path: PathBuf },
    Artwork(PathBuf),
}

pub struct Dialogs {
    tx: Sender<DialogResult>,
    pub results: Receiver<DialogResult>,
    pub rename: Option<(usize, String)>,
    pub new_playlist: Option<String>,
    pub confirm_clear: Option<usize>,
    pub confirm_delete: Option<usize>,
    pub shortcuts_open: bool,
    pub about_open: bool,
    about_icon: Option<egui::TextureHandle>,
}

impl Dialogs {
    pub fn new() -> Self {
        let (tx, results) = crossbeam_channel::unbounded();
        Dialogs { tx, results, rename: None, new_playlist: None, confirm_clear: None, confirm_delete: None, shortcuts_open: false, about_open: false, about_icon: None }
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
        DialogResult::ImportSetlist(path) => setlist_view::import_file(app, path),
        DialogResult::ExportSetlist { format, path } => setlist_view::export_to(app, format, path),
        DialogResult::Artwork(path) => super::inspector::add_artwork_file(app, path),
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

    if app.dialogs.shortcuts_open {
        let mut open = true;
        egui::Window::new("Keyboard shortcuts").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            let cmd = if cfg!(target_os = "macos") { "⌘" } else { "Ctrl+" };
            let alt = if cfg!(target_os = "macos") { "⌥" } else { "Alt+" };
            let rows: Vec<(String, &str)> = vec![
                ("Space / C".into(), "Play / pause"),
                ("X".into(), "Play"),
                ("V".into(), "Stop"),
                ("Z / B".into(), "Previous / next track"),
                ("← / →".into(), "Seek 5 s"),
                ("⇧← / ⇧→".into(), "Previous / next setlist entry"),
                ("+ / − / M".into(), "Volume up / down / mute"),
                ("S / R".into(), "Shuffle / cycle repeat"),
                ("↑ / ↓, Home / End".into(), "Move selection (⇧ extends)"),
                ("Enter".into(), "Play selected"),
                ("Delete / Backspace".into(), "Remove selected"),
                (format!("{alt}↑ / {alt}↓"), "Move selected entries"),
                (format!("{cmd}A"), "Select all"),
                (format!("{cmd}O / {cmd}⇧O"), "Add files / folder"),
                (format!("{cmd}I / {cmd}L / {cmd}P"), "Inspector / setlist / playlist panel"),
                (format!("{cmd}J"), "Scroll to playing track"),
                ("F1".into(), "This help"),
            ];
            egui::Grid::new("shortcuts").num_columns(2).spacing([18.0, 4.0]).show(ui, |ui| {
                for (k, v) in rows {
                    ui.label(RichText::new(k).monospace().color(theme::key()));
                    ui.label(v);
                    ui.end_row();
                }
            });
            ui.add_space(6.0);
            ui.label(RichText::new("Hardware media keys work while Nullamp is in the background.").small().color(theme::text_dim()));
        });
        app.dialogs.shortcuts_open = open;
    }

    if app.dialogs.about_open {
        let mut open = true;
        egui::Window::new("About Nullamp").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            let icon = app.dialogs.about_icon.get_or_insert_with(|| {
                let data = crate::app_icon();
                let image = egui::ColorImage::from_rgba_unmultiplied([data.width as usize, data.height as usize], &data.rgba);
                ui.ctx().load_texture("about-icon", image, egui::TextureOptions::LINEAR)
            });
            ui.horizontal(|ui| {
                ui.add(egui::Image::new(&*icon).fit_to_exact_size(egui::vec2(72.0, 72.0)));
                ui.vertical(|ui| {
                    ui.heading(format!("Nullamp {}", env!("CARGO_PKG_VERSION")));
                    ui.label("A fast, retro-inspired music and DJ mix player.");
                    ui.label(RichText::new(format!("License: {}", env!("CARGO_PKG_LICENSE"))).small().color(theme::text_dim()));
                });
            });
            ui.add_space(6.0);
            let lines = crate::logbuf::tail();
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Log (last {} lines)", lines.len())).small().color(theme::text_dim()));
                if ui.small_button("Copy").clicked() {
                    ui.ctx().copy_text(lines.join("\n"));
                }
            });
            egui::Frame::new().fill(theme::bg()).stroke(egui::Stroke::new(1.0, theme::border())).inner_margin(egui::Margin::same(6)).show(ui, |ui| {
                egui::ScrollArea::both().id_salt("about-log").min_scrolled_width(560.0).max_height(200.0).auto_shrink([false, true]).stick_to_bottom(true).show(ui, |ui| {
                    if lines.is_empty() {
                        ui.label(RichText::new("Nothing logged yet.").small().color(theme::text_faint()));
                    }
                    for l in &lines {
                        ui.add(egui::Label::new(RichText::new(l).font(theme::mono(11.0)).color(theme::text())).extend());
                    }
                });
            });
        });
        app.dialogs.about_open = open;
    }
}
