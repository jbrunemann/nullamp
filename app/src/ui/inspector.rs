//! The metadata inspector: view and edit tags, artwork and technical properties,
//! for one file or a batch, plus online lookups (MusicBrainz, Cover Art Archive).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use egui::{RichText, TextureHandle, Ui, vec2};
use nullamp_core::EntryId;
use nullamp_core::time::format_clock;
use nullamp_meta::{ArtworkKind, Edit, Field, FileMetadata, NewArtwork, WriteOptions};
use nullamp_online::{CoverImage, RecordingMatch};

use super::{App, dialogs, theme};
use crate::workers::{Job, JobResult, OnlineJob, image_hosts_for};

/// Batch edits are limited to keep the editor responsive.
const MAX_TARGETS: usize = 500;

#[derive(Clone, Debug, PartialEq)]
enum Original {
    Same(String),
    Mixed,
}

struct FieldState {
    original: Original,
    value: String,
    touched: bool,
}

struct ArtItem {
    kind: ArtworkKind,
    mime: Option<String>,
    description: Option<String>,
    data: Arc<[u8]>,
    texture: Option<TextureHandle>,
    size: Option<[usize; 2]>,
    decode_request: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LookupTarget {
    #[default]
    File,
    SetlistEntry(EntryId),
}

#[derive(Default)]
struct Lookup {
    target: LookupTarget,
    artist: String,
    title: String,
    request: Option<u64>,
    results: Vec<RecordingMatch>,
    error: Option<String>,
    images_request: Option<u64>,
    images: Vec<CoverImage>,
    /// Thumbnail URL -> (texture, full bytes when fetched).
    thumbs: HashMap<String, (TextureHandle, Arc<[u8]>)>,
    thumb_requests: HashMap<u64, String>,
    chosen_release: Option<(String, String)>,
}

#[derive(Default)]
pub struct InspectorState {
    pub targets: Vec<PathBuf>,
    pub follow_playing: bool,
    request: Option<u64>,
    loaded: BTreeMap<PathBuf, FileMetadata>,
    errors: Vec<(PathBuf, String)>,
    fields: BTreeMap<Field, FieldState>,
    art: Vec<ArtItem>,
    art_touched: bool,
    save_request: Option<u64>,
    status: Option<(String, bool)>,
    last_backup: Option<(PathBuf, PathBuf)>,
    restore_request: Option<u64>,
    lookup: Lookup,
    lookup_open: bool,
}

impl InspectorState {
    fn is_loading(&self) -> bool {
        self.request.is_some() && self.loaded.len() + self.errors.len() < self.targets.len()
    }

    fn dirty(&self) -> bool {
        self.art_touched || self.fields.values().any(|f| f.touched)
    }
}

// ----- Loading -----

pub fn load_from_selection(app: &mut App) {
    let pl = app.player.viewed();
    let mut paths: Vec<PathBuf> = pl.entries().iter().filter(|e| app.selection.ids.contains(&e.id)).map(|e| e.path.clone()).collect();
    if paths.is_empty()
        && let Some(e) = app.player.current_entry()
    {
        paths.push(e.path.clone());
    }
    load_targets(app, paths);
}

pub fn load_targets(app: &mut App, mut paths: Vec<PathBuf>) {
    paths.dedup();
    if paths.len() > MAX_TARGETS {
        app.warn(format!("Inspecting the first {MAX_TARGETS} of {} files", paths.len()));
        paths.truncate(MAX_TARGETS);
    }
    if paths == app.inspector.targets && !app.inspector.dirty() && app.inspector.request.is_some() {
        return;
    }
    let follow = app.inspector.follow_playing;
    let lookup_open = app.inspector.lookup_open;
    app.inspector = InspectorState { follow_playing: follow, lookup_open, ..Default::default() };
    if paths.is_empty() {
        return;
    }
    let request = app.workers.request_id();
    app.inspector.request = Some(request);
    app.inspector.targets = paths.clone();
    for path in paths {
        app.workers.submit(Job::Inspect { request, path });
    }
}

fn reload(app: &mut App) {
    let targets = std::mem::take(&mut app.inspector.targets);
    app.inspector.request = None;
    load_targets(app, targets);
}

fn build_fields(app: &mut App) {
    let st = &mut app.inspector;
    let metas: Vec<&FileMetadata> = st.targets.iter().filter_map(|p| st.loaded.get(p)).collect();
    st.fields.clear();
    for field in Field::ALL {
        let mut values = metas.iter().map(|m| m.fields.get(&field).cloned().unwrap_or_default());
        let first = values.next().unwrap_or_default();
        let original = if values.all(|v| v == first) { Original::Same(first.clone()) } else { Original::Mixed };
        let value = match &original {
            Original::Same(v) => v.clone(),
            Original::Mixed => String::new(),
        };
        st.fields.insert(field, FieldState { original, value, touched: false });
    }
    st.art.clear();
    if let Some(m) = metas.first() {
        for a in &m.artwork {
            st.art.push(ArtItem {
                kind: a.kind,
                mime: a.mime.clone(),
                description: a.description.clone(),
                data: Arc::clone(&a.data),
                texture: None,
                size: a.width.zip(a.height).map(|(w, h)| [w as usize, h as usize]),
                decode_request: None,
            });
        }
    }
    st.art_touched = false;
    let first = metas.first().map(|m| (m.fields.get(&Field::Artist).cloned().unwrap_or_default(), m.fields.get(&Field::Title).cloned().unwrap_or_default()));
    if let Some((artist, title)) = first
        && st.lookup.target == LookupTarget::File
    {
        st.lookup.artist = artist;
        st.lookup.title = title;
    }
    request_art_decodes(app);
}

fn request_art_decodes(app: &mut App) {
    let mut jobs = Vec::new();
    for a in app.inspector.art.iter_mut().filter(|a| a.texture.is_none() && a.decode_request.is_none()) {
        let request = app.workers.request_id();
        a.decode_request = Some(request);
        jobs.push(Job::DecodeImage { request, bytes: Arc::clone(&a.data) });
    }
    for j in jobs {
        app.workers.submit(j);
    }
}

pub fn add_artwork_file(app: &mut App, path: PathBuf) {
    match std::fs::read(&path) {
        Ok(bytes) if bytes.len() <= 15 * 1024 * 1024 => {
            let mime = match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
                Some("png") => "image/png",
                Some("gif") => "image/gif",
                Some("webp") => "image/webp",
                Some("bmp") => "image/bmp",
                _ => "image/jpeg",
            };
            add_artwork(app, Arc::from(bytes), Some(mime.into()));
        }
        Ok(_) => app.error("Image is larger than 15 MB"),
        Err(e) => app.error(format!("Cannot read image: {e}")),
    }
}

fn add_artwork(app: &mut App, data: Arc<[u8]>, mime: Option<String>) {
    let kind = if app.inspector.art.iter().any(|a| a.kind == ArtworkKind::FrontCover) { ArtworkKind::Other } else { ArtworkKind::FrontCover };
    app.inspector.art.push(ArtItem { kind, mime, description: None, data, texture: None, size: None, decode_request: None });
    app.inspector.art_touched = true;
    request_art_decodes(app);
}

/// Store the current setlist as MP3 chapters (via the same safe write path).
pub fn write_chapters(app: &mut App) {
    let (Some(path), Some(setlist)) = (app.mix.path.clone(), app.mix.setlist.clone()) else { return };
    let Some(stamp) = app.now.meta.as_ref().filter(|m| m.path == path).map(|m| m.stamp) else {
        app.warn("File information is still loading; try again in a moment");
        return;
    };
    let request = app.workers.request_id();
    app.inspector.save_request = Some(request);
    let edit = Edit { chapters: Some(setlist), ..Default::default() };
    app.workers.submit(Job::WriteTags { request, files: vec![(path, stamp)], edit, options: write_options(app) });
    app.info("Writing chapters…");
}

fn write_options(app: &App) -> WriteOptions {
    WriteOptions {
        backup_dir: app.settings.backup_before_tag_writes.then(|| app.store.paths.tag_backups()),
        backup_budget_bytes: app.settings.backup_budget_mb * 1024 * 1024,
    }
}

fn save(app: &mut App) {
    let st = &app.inspector;
    let mut edit = Edit::default();
    for (field, f) in &st.fields {
        if f.touched {
            let v = f.value.trim();
            edit.fields.insert(*field, if v.is_empty() { None } else { Some(v.to_owned()) });
        }
    }
    if st.art_touched {
        edit.artwork = Some(
            st.art.iter().map(|a| NewArtwork { kind: a.kind, mime: a.mime.clone(), description: a.description.clone(), data: Arc::clone(&a.data) }).collect(),
        );
    }
    if edit.is_empty() {
        return;
    }
    let files: Vec<(PathBuf, nullamp_meta::FileStamp)> = st.targets.iter().filter_map(|p| st.loaded.get(p).map(|m| (p.clone(), m.stamp))).collect();
    let request = app.workers.request_id();
    app.inspector.save_request = Some(request);
    app.inspector.status = Some((format!("Saving {} file{}…", files.len(), if files.len() == 1 { "" } else { "s" }), false));
    let options = write_options(app);
    app.workers.submit(Job::WriteTags { request, files, edit, options });
}

pub fn on_result(app: &mut App, r: JobResult) {
    match r {
        JobResult::Inspected { request, path, meta } if app.inspector.request == Some(request) => {
            match meta {
                Ok(m) => {
                    app.inspector.loaded.insert(path, m);
                }
                Err(e) => app.inspector.errors.push((path, e)),
            }
            if !app.inspector.is_loading() {
                build_fields(app);
            }
        }
        JobResult::Image { request, thumb } => {
            let ctx = app.ctx.clone();
            if let Some(a) = app.inspector.art.iter_mut().find(|a| a.decode_request == Some(request)) {
                match thumb {
                    Some(t) => {
                        let img = egui::ColorImage::from_rgba_unmultiplied(t.size, &t.rgba);
                        a.size = a.size.or(Some(t.size));
                        a.texture = Some(ctx.load_texture(format!("art-{request}"), img, egui::TextureOptions::LINEAR));
                    }
                    None => a.description = Some(a.description.clone().unwrap_or_else(|| "cannot display this image".into())),
                }
            }
        }
        JobResult::Written { request, results } => {
            let is_inspector_save = app.inspector.save_request == Some(request);
            if is_inspector_save {
                app.inspector.save_request = None;
            }
            let ok: Vec<PathBuf> = results.iter().filter(|(_, r)| r.is_ok()).map(|(p, _)| p.clone()).collect();
            let failed: Vec<String> = results.iter().filter_map(|(p, r)| r.as_ref().err().map(|e| format!("{}: {e}", p.file_name().unwrap_or_default().to_string_lossy()))).collect();
            if let Some((p, Ok(report))) = results.iter().find(|(_, r)| r.as_ref().is_ok_and(|r| r.backup.is_some())) {
                app.inspector.last_backup = report.backup.clone().map(|b| (b, p.clone()));
            }
            let skipped: Vec<String> = results.iter().filter_map(|(_, r)| r.as_ref().ok()?.backup_skipped.clone()).collect();
            let unsupported: Vec<&str> = results
                .iter()
                .filter_map(|(_, r)| r.as_ref().ok())
                .flat_map(|r| r.unsupported.iter().map(|f| f.label()))
                .collect();
            let msg = if failed.is_empty() {
                let mut m = format!("Saved {} file{}", ok.len(), if ok.len() == 1 { "" } else { "s" });
                if !unsupported.is_empty() {
                    m.push_str(&format!(" (not supported by this format: {})", unsupported.join(", ")));
                }
                if let Some(s) = skipped.first() {
                    m.push_str(&format!("; no backup: {s}"));
                }
                (m, false)
            } else {
                (format!("{} saved, {} failed: {}", ok.len(), failed.len(), failed.join("; ")), true)
            };
            app.inspector.status = Some(msg.clone());
            if msg.1 { app.error(msg.0) } else { app.info(msg.0) }
            // Refresh what is shown everywhere from the files as written.
            app.workers.scan(ok.clone());
            if app.player.current.as_ref().is_some_and(|c| ok.contains(&c.path)) {
                let c = app.player.current.clone().unwrap();
                app.workers.submit(Job::NowPlaying { token: c.token, path: c.path });
            }
            if is_inspector_save {
                reload(app);
            }
        }
        JobResult::Restored { request, target, result } if app.inspector.restore_request == Some(request) => {
            app.inspector.restore_request = None;
            match result {
                Ok(()) => {
                    app.info(format!("Restored {} from backup", target.display()));
                    app.inspector.last_backup = None;
                    app.workers.scan(vec![target]);
                    reload(app);
                }
                Err(e) => app.error(format!("Restore failed: {e}")),
            }
        }
        JobResult::Recordings { request, result } if app.inspector.lookup.request == Some(request) => {
            let l = &mut app.inspector.lookup;
            l.request = None;
            match result {
                Ok(r) if r.is_empty() => l.error = Some("No matches on MusicBrainz.".into()),
                Ok(r) => l.results = r,
                Err(e) => l.error = Some(e.to_string()),
            }
        }
        JobResult::ReleaseImages { request, result } if app.inspector.lookup.images_request == Some(request) => {
            app.inspector.lookup.images_request = None;
            match result {
                Ok(images) if images.is_empty() => app.inspector.lookup.error = Some("This release has no artwork in the Cover Art Archive.".into()),
                Ok(images) => {
                    for img in images.iter().take(8) {
                        let request = app.workers.request_id();
                        app.inspector.lookup.thumb_requests.insert(request, img.thumbnail.clone());
                        app.workers.submit_online(OnlineJob::FetchImage { request, url: img.thumbnail.clone(), hosts: image_hosts_for(&img.thumbnail) });
                    }
                    app.inspector.lookup.images = images;
                }
                Err(e) => app.inspector.lookup.error = Some(e.to_string()),
            }
        }
        JobResult::OnlineImage { request, url, result } => {
            if app.inspector.lookup.thumb_requests.remove(&request).is_some() {
                match result {
                    Ok((bytes, t)) => {
                        let img = egui::ColorImage::from_rgba_unmultiplied(t.size, &t.rgba);
                        let tex = app.ctx.load_texture(format!("lookup-{request}"), img, egui::TextureOptions::LINEAR);
                        app.inspector.lookup.thumbs.insert(url, (tex, bytes));
                    }
                    Err(e) => log::debug!("thumbnail {url}: {e}"),
                }
            } else {
                super::setlist_view::on_entry_art(app, request, url, result);
            }
        }
        _ => {}
    }
}

// ----- The panel -----

pub fn show(app: &mut App, ui: &mut Ui) {
    ui.horizontal(|ui| {
        if ui.checkbox(&mut app.inspector.follow_playing, "Follow playing track").changed() && app.inspector.follow_playing
            && let Some(c) = app.player.current.clone()
        {
            load_targets(app, vec![c.path]);
        }
        if ui.small_button("Inspect selection").clicked() {
            app.inspector.follow_playing = false;
            load_from_selection(app);
        }
    });
    ui.add_space(4.0);
    let st = &app.inspector;
    if st.targets.is_empty() {
        ui.label(RichText::new("Select tracks in the playlist to view and edit their metadata.").color(theme::text_dim()));
        return;
    }
    if st.is_loading() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new(format!("Reading {} file{}…", st.targets.len(), if st.targets.len() == 1 { "" } else { "s" })).color(theme::text_dim()));
        });
        return;
    }
    for (p, e) in st.errors.clone() {
        ui.label(RichText::new(format!("⚠ {}: {e}", p.file_name().unwrap_or_default().to_string_lossy())).small().color(theme::warn()));
    }
    if st.loaded.is_empty() {
        return;
    }

    egui::ScrollArea::vertical().id_salt("inspector").auto_shrink([false, false]).show(ui, |ui| {
        header(app, ui);
        ui.add_space(6.0);
        fields(app, ui);
        ui.add_space(6.0);
        actions(app, ui);
        ui.add_space(8.0);
        artwork(app, ui);
        ui.add_space(8.0);
        lookup(app, ui);
        ui.add_space(8.0);
        technical(app, ui);
    });
}

fn header(app: &App, ui: &mut Ui) {
    let st = &app.inspector;
    if st.targets.len() == 1 {
        let p = &st.targets[0];
        ui.add(egui::Label::new(RichText::new(p.file_name().unwrap_or_default().to_string_lossy()).strong()).truncate());
        ui.add(egui::Label::new(RichText::new(p.parent().map(|d| d.display().to_string()).unwrap_or_default()).small().color(theme::text_faint())).truncate());
        if let Some(m) = st.loaded.get(p)
            && !m.writable
        {
            ui.label(RichText::new("This file type cannot store tags; fields are read-only.").small().color(theme::warn()));
        }
    } else {
        ui.label(RichText::new(format!("{} files selected", st.targets.len())).strong());
        ui.label(RichText::new("Fields showing “multiple values” differ between files. Only the fields you change are written.").small().color(theme::text_dim()));
    }
}

fn fields(app: &mut App, ui: &mut Ui) {
    let writable = app.inspector.loaded.values().all(|m| m.writable);
    egui::Grid::new("fields").num_columns(2).spacing([8.0, 4.0]).min_col_width(80.0).show(ui, |ui| {
        let mut pair_done = false;
        for field in Field::ALL {
            if pair_done {
                pair_done = false;
                continue;
            }
            let Some(f) = app.inspector.fields.get_mut(&field) else { continue };
            let label_color = if f.touched { theme::accent() } else { theme::text_dim() };
            // Number pairs on one row: "Track 3 of 12".
            if matches!(field, Field::TrackNumber | Field::DiscNumber) {
                let total = if field == Field::TrackNumber { Field::TrackTotal } else { Field::DiscTotal };
                ui.label(RichText::new(field.label()).color(label_color));
                ui.horizontal(|ui| {
                    number_edit(ui, app.inspector.fields.get_mut(&field).unwrap(), writable);
                    ui.label(RichText::new("of").color(theme::text_dim()));
                    number_edit(ui, app.inspector.fields.get_mut(&total).unwrap(), writable);
                });
                ui.end_row();
                pair_done = true;
                continue;
            }
            ui.label(RichText::new(field.label()).color(label_color));
            ui.vertical(|ui| {
                let hint = if f.original == Original::Mixed && !f.touched { "‹multiple values›" } else { "" };
                let edit = if field.is_multiline() {
                    egui::TextEdit::multiline(&mut f.value).desired_rows(2)
                } else {
                    egui::TextEdit::singleline(&mut f.value)
                };
                let r = ui.add_enabled(writable, edit.desired_width(f32::INFINITY).hint_text(hint));
                if r.changed() {
                    f.touched = match &f.original {
                        Original::Same(v) => *v != f.value,
                        Original::Mixed => true,
                    };
                }
                if f.touched
                    && let Err(e) = field.validate(&f.value)
                {
                    ui.label(RichText::new(e).small().color(theme::err()));
                }
            });
            ui.end_row();
        }
    });
}

fn number_edit(ui: &mut Ui, f: &mut FieldState, writable: bool) {
    let hint = if f.original == Original::Mixed && !f.touched { "‹mixed›" } else { "" };
    let r = ui.add_enabled(writable, egui::TextEdit::singleline(&mut f.value).desired_width(44.0).hint_text(hint));
    if r.changed() {
        f.touched = match &f.original {
            Original::Same(v) => *v != f.value,
            Original::Mixed => true,
        };
    }
}

fn actions(app: &mut App, ui: &mut Ui) {
    let st = &app.inspector;
    let valid = st.fields.iter().all(|(field, f)| !f.touched || field.validate(&f.value).is_ok());
    let dirty = st.dirty();
    let saving = st.save_request.is_some();
    ui.horizontal(|ui| {
        if ui.add_enabled(dirty && valid && !saving, egui::Button::new(RichText::new("Save").strong())).on_hover_text("Write the changed fields to the file(s)").clicked() {
            save(app);
        }
        if ui.add_enabled(dirty && !saving, egui::Button::new("Cancel")).on_hover_text("Discard changes").clicked() {
            build_fields(app);
            app.inspector.status = None;
        }
        if ui.button("Reload").on_hover_text("Read the file(s) again").clicked() {
            reload(app);
        }
        if saving {
            ui.spinner();
        }
    });
    if let Some((msg, err)) = &app.inspector.status {
        ui.label(RichText::new(msg).small().color(if *err { theme::err() } else { theme::ok() }));
        if *err && msg.contains("changed on disk") && ui.small_button("Reload and re-apply manually").clicked() {
            reload(app);
        }
    }
    if let Some((backup, target)) = app.inspector.last_backup.clone() {
        ui.horizontal(|ui| {
            ui.label(RichText::new("A backup of the previous version was kept.").small().color(theme::text_faint()));
            if ui.small_button("Restore it").clicked() {
                let request = app.workers.request_id();
                app.inspector.restore_request = Some(request);
                app.workers.submit(Job::RestoreBackup { request, backup, target });
            }
        });
    }
}

fn artwork(app: &mut App, ui: &mut Ui) {
    let writable = app.inspector.loaded.values().all(|m| m.writable);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Artwork").strong());
        if app.inspector.art_touched {
            ui.label(RichText::new("changed").small().color(theme::accent()));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add_enabled(writable, egui::Button::new("Import…").small()).clicked() {
                dialogs::pick_artwork(app);
            }
        });
    });
    if app.inspector.targets.len() > 1 {
        ui.label(RichText::new("Showing the first file's artwork. Changing it replaces artwork in all selected files.").small().color(theme::text_faint()));
    }
    if app.inspector.art.is_empty() {
        ui.label(RichText::new("No embedded images.").small().color(theme::text_dim()));
        return;
    }
    let mut remove = None;
    let mut make_front = None;
    ui.horizontal_wrapped(|ui| {
        for (i, a) in app.inspector.art.iter().enumerate() {
            ui.vertical(|ui| {
                ui.set_width(104.0);
                let (rect, _) = ui.allocate_exact_size(vec2(104.0, 104.0), egui::Sense::hover());
                ui.painter().rect_filled(rect, 3.0, theme::LCD_BG);
                if let Some(t) = &a.texture {
                    let s = t.size_vec2();
                    let scale = (rect.width() / s.x).min(rect.height() / s.y);
                    ui.painter().image(t.id(), egui::Rect::from_center_size(rect.center(), s * scale), egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                }
                let dims = a.size.map(|[w, h]| format!("{w}×{h}")).unwrap_or_default();
                let kb = a.data.len() / 1024;
                ui.label(RichText::new(format!("{} · {dims} · {kb} KB", a.kind.label())).small().color(theme::text_dim()));
                ui.horizontal(|ui| {
                    if a.kind != ArtworkKind::FrontCover && ui.add_enabled(writable, egui::Button::new("Front").small()).on_hover_text("Use as front cover").clicked() {
                        make_front = Some(i);
                    }
                    if ui.add_enabled(writable, egui::Button::new("Remove").small()).clicked() {
                        remove = Some(i);
                    }
                });
            });
        }
    });
    if let Some(i) = make_front {
        for (j, a) in app.inspector.art.iter_mut().enumerate() {
            if j == i {
                a.kind = ArtworkKind::FrontCover;
            } else if a.kind == ArtworkKind::FrontCover {
                a.kind = ArtworkKind::Other;
            }
        }
        app.inspector.art.swap(0, i);
        app.inspector.art_touched = true;
    }
    if let Some(i) = remove {
        app.inspector.art.remove(i);
        app.inspector.art_touched = true;
    }
}

/// Open the lookup for a setlist entry (called from the setlist panel).
pub fn lookup_for_entry(app: &mut App, id: EntryId, artist: String, title: String) {
    app.side_tab = super::SideTab::Inspector;
    app.side_open = true;
    app.inspector.lookup_open = true;
    let l = &mut app.inspector.lookup;
    *l = Lookup { target: LookupTarget::SetlistEntry(id), artist, title, ..Default::default() };
    start_lookup(app);
}

fn start_lookup(app: &mut App) {
    if !app.settings.online_lookups {
        app.inspector.lookup.error = Some("Online lookups are off (see status bar).".into());
        return;
    }
    let request = app.workers.request_id();
    let l = &mut app.inspector.lookup;
    l.request = Some(request);
    l.results.clear();
    l.images.clear();
    l.error = None;
    l.chosen_release = None;
    app.workers.submit_online(OnlineJob::RecordingSearch { request, artist: l.artist.clone(), title: l.title.clone() });
}

fn lookup(app: &mut App, ui: &mut Ui) {
    let open = egui::CollapsingHeader::new(RichText::new("Find metadata online").strong())
        .id_salt("lookup")
        .open(if app.inspector.lookup_open { Some(true) } else { None })
        .show(ui, |ui| lookup_body(app, ui));
    app.inspector.lookup_open = open.fully_open() || app.inspector.lookup_open && !open.header_response.clicked();
}

fn lookup_body(app: &mut App, ui: &mut Ui) {
    if let LookupTarget::SetlistEntry(_) = app.inspector.lookup.target {
        ui.horizontal(|ui| {
            ui.label(RichText::new("For a setlist entry").small().color(theme::now()));
            if ui.small_button("switch to file").clicked() {
                app.inspector.lookup = Lookup::default();
                build_fields(app);
            }
        });
    }
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut app.inspector.lookup.artist).hint_text("Artist").desired_width(120.0));
        ui.add(egui::TextEdit::singleline(&mut app.inspector.lookup.title).hint_text("Title").desired_width(ui.available_width() - 70.0));
        if ui.button("Search").clicked() {
            start_lookup(app);
        }
    });
    let l = &app.inspector.lookup;
    if l.request.is_some() || l.images_request.is_some() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new("Asking MusicBrainz…").small().color(theme::text_dim()));
        });
    }
    if let Some(e) = &l.error {
        ui.label(RichText::new(e).small().color(theme::warn()));
    }
    let file_duration = app.inspector.loaded.values().next().and_then(|m| m.technical.duration);
    let results = l.results.clone();
    let mut use_fields = None;
    let mut want_art = None;
    for r in results.iter().take(8) {
        egui::Frame::new().fill(theme::raised()).corner_radius(3.0).inner_margin(egui::Margin::same(6)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(RichText::new(format!("{} — {}", r.artist, r.title)).strong()).truncate());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.hyperlink_to("↗", &r.url).on_hover_text("Open on MusicBrainz");
                    ui.label(RichText::new(format!("{}%", r.score)).small().color(theme::text_faint()));
                });
            });
            let len = r.length.map(format_clock).unwrap_or_else(|| "?".into());
            let matches = matches!((file_duration, r.length), (Some(a), Some(b)) if (a - b).abs() < 3.0);
            let mut sub = format!("length {len}{}", if matches { " ✓ matches" } else { "" });
            if let Some(d) = &r.disambiguation {
                sub.push_str(&format!(" · {d}"));
            }
            ui.label(RichText::new(sub).small().color(if matches { theme::ok() } else { theme::text_dim() }));
            for rel in r.releases.iter().take(3) {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{} {}", rel.title, rel.date.as_deref().map(|d| format!("({d})")).unwrap_or_default())).small());
                    if ui.small_button("Use").on_hover_text("Fill fields from this recording and release (not saved until you press Save)").clicked() {
                        use_fields = Some((r.clone(), rel.clone()));
                    }
                    if ui.small_button("Artwork").clicked() {
                        want_art = Some((rel.id.clone(), rel.title.clone()));
                    }
                });
            }
        });
    }
    if let Some((r, rel)) = use_fields {
        apply_suggestion(app, &r, &rel);
    }
    if let Some((id, title)) = want_art {
        let request = app.workers.request_id();
        app.inspector.lookup.images_request = Some(request);
        app.inspector.lookup.images.clear();
        app.inspector.lookup.chosen_release = Some((id.clone(), title));
        app.workers.submit_online(OnlineJob::ReleaseArtwork { request, release_id: id });
    }
    let images = app.inspector.lookup.images.clone();
    if !images.is_empty() {
        ui.label(RichText::new(format!("Artwork for “{}”", app.inspector.lookup.chosen_release.as_ref().map(|r| r.1.as_str()).unwrap_or(""))).small());
        let mut chosen = None;
        ui.horizontal_wrapped(|ui| {
            for img in &images {
                ui.vertical(|ui| {
                    let (rect, resp) = ui.allocate_exact_size(vec2(84.0, 84.0), egui::Sense::click());
                    ui.painter().rect_filled(rect, 3.0, theme::LCD_BG);
                    if let Some((tex, _)) = app.inspector.lookup.thumbs.get(&img.thumbnail) {
                        let s = tex.size_vec2();
                        let scale = (rect.width() / s.x).min(rect.height() / s.y);
                        ui.painter().image(tex.id(), egui::Rect::from_center_size(rect.center(), s * scale), egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                    } else {
                        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "…", theme::prop(14.0), theme::text_faint());
                    }
                    let label = if img.types.is_empty() { "image".to_owned() } else { img.types.join(", ") };
                    let resp = resp.on_hover_text(format!("{label}\nClick to use"));
                    if resp.clicked() {
                        chosen = Some(img.clone());
                    }
                });
            }
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new("Images: Cover Art Archive.").small().color(theme::text_faint()));
            if let Some(img) = images.first() {
                ui.hyperlink_to(RichText::new("release page").small(), &img.source_page);
            }
        });
        if let Some(img) = chosen {
            use_image(app, &img);
        }
    }
    ui.label(RichText::new("Metadata: MusicBrainz (CC0). Suggestions are not written until you save.").small().color(theme::text_faint()));
}

fn apply_suggestion(app: &mut App, r: &RecordingMatch, rel: &nullamp_online::ReleaseRef) {
    match app.inspector.lookup.target {
        LookupTarget::File => {
            let mut set = |field: Field, value: &str| {
                if let Some(f) = app.inspector.fields.get_mut(&field)
                    && !value.trim().is_empty()
                {
                    f.value = value.to_owned();
                    f.touched = f.original != Original::Same(f.value.clone());
                }
            };
            set(Field::Title, &r.title);
            set(Field::Artist, &r.artist);
            set(Field::Album, &rel.title);
            if let Some(d) = &rel.date {
                set(Field::Date, d);
            }
            app.info("Fields filled from MusicBrainz; review and press Save");
        }
        LookupTarget::SetlistEntry(id) => {
            super::setlist_view::apply_lookup_to_entry(app, id, r, rel);
        }
    }
}

fn use_image(app: &mut App, img: &CoverImage) {
    match app.inspector.lookup.target {
        LookupTarget::File => {
            // Use the full-size image when fetched; the thumbnail otherwise.
            let Some((_, bytes)) = app.inspector.lookup.thumbs.get(&img.thumbnail) else { return };
            let bytes = Arc::clone(bytes);
            add_artwork(app, bytes, Some("image/jpeg".into()));
            app.info("Artwork added; press Save to write it");
        }
        LookupTarget::SetlistEntry(id) => super::setlist_view::set_entry_artwork(app, id, img.full.clone()),
    }
}

fn technical(app: &mut App, ui: &mut Ui) {
    let Some(m) = app.inspector.targets.first().and_then(|p| app.inspector.loaded.get(p)).cloned() else { return };
    egui::CollapsingHeader::new(RichText::new("Technical (read-only)").strong()).id_salt("technical").default_open(true).show(ui, |ui| {
        let t = &m.technical;
        egui::Grid::new("tech").num_columns(2).spacing([10.0, 2.0]).show(ui, |ui| {
            let mut row = |k: &str, v: String| {
                ui.label(RichText::new(k).small().color(theme::text_dim()));
                ui.label(RichText::new(v).small().monospace());
                ui.end_row();
            };
            row("Format", t.format.clone());
            row("Duration", t.duration.map(nullamp_core::time::format_precise).unwrap_or_else(|| "unknown".into()));
            let mode = match t.vbr {
                Some(true) => " (VBR)",
                Some(false) => " (CBR)",
                None => "",
            };
            row("Bitrate", t.audio_bitrate_kbps.or(t.bitrate_kbps).map(|b| format!("{b} kbps{mode}")).unwrap_or_else(|| "unknown".into()));
            row("Sample rate", t.sample_rate.map(|r| format!("{r} Hz")).unwrap_or_else(|| "unknown".into()));
            row("Channels", t.channels.map(|c| c.to_string()).unwrap_or_else(|| "unknown".into()));
            if let Some(b) = t.bit_depth {
                row("Bit depth", format!("{b} bit"));
            }
            row("File size", format!("{:.1} MB", t.file_size as f64 / 1_000_000.0));
            row("Tags", if t.tag_types.is_empty() { "none".into() } else { t.tag_types.join(", ") });
            row("Artwork", format!("{} image{}", m.artwork.len(), if m.artwork.len() == 1 { "" } else { "s" }));
            row("Chapters", m.chapters.len().to_string());
        });
        if !m.chapters.is_empty() {
            egui::CollapsingHeader::new(RichText::new(format!("Embedded chapters ({})", m.chapters.len())).small()).id_salt("chapters").show(ui, |ui| {
                for c in &m.chapters {
                    ui.label(RichText::new(format!("{}  {}{}", format_clock(c.start), c.artist.as_deref().map(|a| format!("{a} — ")).unwrap_or_default(), c.title.as_deref().unwrap_or("(untitled)"))).small().monospace());
                }
            });
        }
        if let Some(cue) = &m.embedded_cue {
            egui::CollapsingHeader::new(RichText::new("Cue sheet in tags").small()).id_salt("embedded-cue").show(ui, |ui| {
                ui.label(RichText::new(cue.chars().take(4000).collect::<String>()).small().monospace());
            });
        }
        if !m.other_items.is_empty() {
            egui::CollapsingHeader::new(RichText::new(format!("Other tag items ({}) · preserved when saving", m.other_items.len())).small()).id_salt("other-items").show(ui, |ui| {
                egui::Grid::new("other").num_columns(2).spacing([10.0, 2.0]).show(ui, |ui| {
                    for (k, v) in &m.other_items {
                        ui.label(RichText::new(k).small().color(theme::text_dim()));
                        ui.add(egui::Label::new(RichText::new(v).small()).truncate());
                        ui.end_row();
                    }
                });
            });
        }
    });
}
