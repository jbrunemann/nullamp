//! The metadata inspector: view and edit tags, artwork and technical properties,
//! for one file or a batch.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use egui::{RichText, TextureHandle, Ui, vec2};
use nullamp_audio::DetectedKey;
use nullamp_core::time::format_clock;
use nullamp_meta::{ArtworkKind, Edit, Field, FileMetadata, NewArtwork, WriteOptions};

use super::{App, dialogs, theme};
use crate::workers::{Job, JobResult};

/// Batch edits are limited to keep the editor responsive.
const MAX_TARGETS: usize = 500;

/// The comment box grows to fill the visible panel, so the next section only appears on scrolling.
/// The cap keeps it sensible on very tall screens, and the floor keeps it usable in a short panel.
const COMMENT_MAX_HEIGHT: f32 = 480.0;
const COMMENT_MIN_HEIGHT: f32 = 44.0;
/// Room kept below the comment for the Save / Cancel row, so the buttons stay in view.
const ACTIONS_RESERVE: f32 = 44.0;

/// Key and BPM hold a few characters, so their fields are short and what the audio suggests for
/// them sits on the same row. The field gives way to the suggestion in a narrow panel, down to the floor.
const SHORT_FIELD: f32 = 96.0;
const SHORT_FIELD_MIN: f32 = 56.0;

/// Where the scrolling content starts and how tall the visible panel is.
#[derive(Clone, Copy)]
struct Viewport {
    content_top: f32,
    height: f32,
}

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

#[derive(Default)]
pub struct InspectorState {
    pub targets: Vec<PathBuf>,
    request: Option<u64>,
    loaded: BTreeMap<PathBuf, FileMetadata>,
    /// What rekordbox knows of each file, empty when it knows nothing.
    rekordbox: BTreeMap<PathBuf, Vec<(String, String)>>,
    errors: Vec<(PathBuf, String)>,
    fields: BTreeMap<Field, FieldState>,
    art: Vec<ArtItem>,
    art_touched: bool,
    save_request: Option<u64>,
    status: Option<(String, bool)>,
    last_backup: Option<(PathBuf, PathBuf)>,
    restore_request: Option<u64>,
    cover_request: Option<u64>,
    key_detection: KeyDetection,
}

/// The key of the one open file found by listening, asked for once it is clear the file has no key tag.
#[derive(Default)]
enum KeyDetection {
    #[default]
    NotAsked,
    Asked,
    /// `None`: no key found.
    Done(Option<DetectedKey>),
}

/// What the audio suggests for a short field, as a button beside it.
struct Suggestion {
    value: String,
    /// The button's text, and a shorter one for when the row is narrow.
    long: String,
    short: String,
    tooltip: Option<&'static str>,
}

impl InspectorState {
    /// What the field holds as typed so far.
    #[cfg(test)]
    pub(crate) fn field_value(&self, field: Field) -> &str {
        self.fields.get(&field).map_or("", |f| f.value.as_str())
    }

    fn is_loading(&self) -> bool {
        self.request.is_some() && self.loaded.len() + self.errors.len() < self.targets.len()
    }

    fn dirty(&self) -> bool {
        self.art_touched || self.fields.values().any(|f| f.touched)
    }
}

// ----- Loading -----

/// What a freshly opened panel shows: the playing track when following it, else the selection.
pub fn load_on_open(app: &mut App) {
    match app.player.current.clone() {
        Some(c) if app.settings.inspector_follows_playing => load_targets(app, vec![c.path]),
        _ => load_from_selection(app),
    }
}

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
    app.inspector = InspectorState::default();
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
    push_artwork(app, kind, data, mime);
}

fn push_artwork(app: &mut App, kind: ArtworkKind, data: Arc<[u8]>, mime: Option<String>) {
    app.inspector.art.push(ArtItem { kind, mime, description: None, data, texture: None, size: None, decode_request: None });
    app.inspector.art_touched = true;
    request_art_decodes(app);
}

/// Store the current setlist as MP3 chapters (via the same safe write path).
pub fn write_chapters(app: &mut App) {
    let Some(setlist) = app.mix.setlist.clone() else { return };
    write_setlist(app, Edit { chapters: Some(setlist), ..Default::default() }, "Writing chapters…");
}

/// Replace the playing file's comment with a cue sheet of its setlist, which discovery reads
/// back as an embedded cue sheet (and other players that know the convention do too).
pub fn write_cue_comment(app: &mut App) {
    let (Some(path), Some(setlist)) = (&app.mix.path, &app.mix.setlist) else { return };
    let audio_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let text = nullamp_core::cue::write(setlist, &audio_name);
    let edit = Edit { fields: BTreeMap::from([(Field::Comment, Some(text.trim_end().to_owned()))]), ..Default::default() };
    write_setlist(app, edit, "Writing the cue sheet…");
}

fn write_setlist(app: &mut App, edit: Edit, status: &str) {
    let Some(path) = app.mix.path.clone() else { return };
    let Some(stamp) = app.now.meta.as_ref().filter(|m| m.path == path).map(|m| m.stamp) else {
        app.warn("File information is still loading; try again in a moment");
        return;
    };
    let request = app.workers.request_id();
    app.inspector.save_request = Some(request);
    app.workers.submit(Job::WriteTags { request, files: vec![(path, stamp)], edit, options: write_options(app) });
    app.info(status);
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

/// The key found for `path`; dropped when the panel has moved on to other files.
pub fn on_key(app: &mut App, path: PathBuf, result: Result<Option<DetectedKey>, String>) {
    if app.inspector.targets != [path] || !matches!(app.inspector.key_detection, KeyDetection::Asked) {
        return;
    }
    match result {
        Ok(key) => app.inspector.key_detection = KeyDetection::Done(key),
        Err(e) => {
            log::debug!("key: {e}");
            app.inspector.key_detection = KeyDetection::Done(None);
        }
    }
}

pub fn on_result(app: &mut App, r: JobResult) {
    match r {
        JobResult::Inspected { request, path, meta, rekordbox } if app.inspector.request == Some(request) => {
            match meta {
                Ok(m) => {
                    app.inspector.rekordbox.insert(path.clone(), rekordbox);
                    app.inspector.loaded.insert(path, m);
                }
                Err(e) => app.inspector.errors.push((path, e)),
            }
            if !app.inspector.is_loading() {
                build_fields(app);
            }
        }
        JobResult::TrackCover { request, result } if app.inspector.cover_request == Some(request) => {
            app.inspector.cover_request = None;
            match result {
                Ok(Some(c)) if app.inspector.art.iter().any(|a| *a.data == *c.data) => app.info("That cover is already in the list"),
                Ok(Some(c)) => {
                    // Never the front cover: the user chooses that with "Set as front cover".
                    push_artwork(app, ArtworkKind::Other, c.data, Some(c.mime.into()));
                    app.info(format!("Added a cover found online: {}", c.source));
                }
                Ok(None) => app.info("No cover found online for this artist and title"),
                Err(e) => app.error(format!("Online lookup failed: {e}")),
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
            app.pl_view.art.forget(&ok);
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
                    app.pl_view.art.forget(std::slice::from_ref(&target));
                    app.workers.scan(vec![target]);
                    reload(app);
                }
                Err(e) => app.error(format!("Restore failed: {e}")),
            }
        }
        _ => {}
    }
}

// ----- The panel -----

pub fn show(app: &mut App, ui: &mut Ui) {
    ui.horizontal(|ui| {
        let follow = ui.checkbox(&mut app.settings.inspector_follows_playing, "Follow playing track")
            .on_hover_text("Always show the track that is playing, whatever is selected in the playlist");
        if follow.changed() {
            app.mark_settings_dirty();
            if app.settings.inspector_follows_playing
                && let Some(c) = app.player.current.clone()
            {
                load_targets(app, vec![c.path]);
            }
        }
        if ui.small_button("Inspect selection").on_hover_text("Stop following playback and show the tracks selected in the playlist").clicked() {
            app.settings.inspector_follows_playing = false;
            app.mark_settings_dirty();
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

    // The bar gets its own gutter so it does not cover right-aligned buttons and fields.
    ui.spacing_mut().scroll.floating_allocated_width = theme::SCROLL_GUTTER;
    let panel_height = ui.available_height();
    egui::ScrollArea::vertical().id_salt("inspector").auto_shrink([false, false]).show(ui, |ui| {
        let viewport = Viewport { content_top: ui.cursor().top(), height: panel_height };
        header(app, ui);
        ui.add_space(6.0);
        fields(app, ui, viewport);
        ui.add_space(6.0);
        actions(app, ui);
        ui.add_space(8.0);
        artwork(app, ui);
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

fn fields(app: &mut App, ui: &mut Ui, viewport: Viewport) {
    let writable = app.inspector.loaded.values().all(|m| m.writable);
    // A grid column is as wide as what it held last, so the room beside a short field is measured to the panel's visible edge.
    let right_edge = ui.clip_rect().right();
    ask_for_key(app, writable);
    // What listening to the file suggests, offered beside the empty field. The tempo is the
    // playing file's, so it is only offered when that is the file open here.
    let bpm_suggestion = match (app.inspector.targets.as_slice(), app.player.current.as_ref(), app.now.tempo) {
        ([target], Some(c), Some(t)) if *target == c.path => {
            let bpm = super::format_bpm(t.bpm);
            Some(Suggestion { long: format!("Detected {bpm} BPM: use it"), short: format!("Use {bpm}"), value: bpm, tooltip: None })
        }
        _ => None,
    };
    let key_suggestion = match app.inspector.key_detection {
        KeyDetection::Done(Some(k)) => {
            let name = nullamp_core::Key::new(k.tonic, k.minor).display(app.key_notation());
            Some(Suggestion { long: format!("Detected {name}: use it"), short: format!("Use {name}"), value: name, tooltip: Some("Found by listening to the track. Usually right, but check it by ear.") })
        }
        _ => None,
    };
    // Both rows give their buttons the long text or the short one together, by the wider of the two.
    let widest_long = [&bpm_suggestion, &key_suggestion].into_iter().flatten().map(|s| button_width(ui, &s.long)).fold(0.0, f32::max);
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
            if field == Field::TrackNumber {
                let total = Field::TrackTotal;
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
            let label = RichText::new(field.label()).color(label_color);
            if field.is_multiline() {
                ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| ui.label(label));
            } else {
                ui.label(label);
            }
            ui.vertical(|ui| {
                let short = matches!(field, Field::Key | Field::Bpm);
                let hint = match (f.original == Original::Mixed && !f.touched, short) {
                    (false, _) => "",
                    (true, false) => "‹multiple values›",
                    (true, true) => "‹mixed›",
                };
                let r = if field.is_multiline() {
                    let above = ui.cursor().top() - viewport.content_top;
                    let height = (viewport.height - above - ACTIONS_RESERVE).clamp(COMMENT_MIN_HEIGHT, COMMENT_MAX_HEIGHT);
                    let edit = egui::TextEdit::multiline(&mut f.value).min_size(vec2(0.0, height));
                    ui.add_enabled(writable, edit.desired_width(f32::INFINITY).hint_text(hint))
                } else if short {
                    let suggestion = match field {
                        Field::Key => key_suggestion.as_ref(),
                        _ => bpm_suggestion.as_ref(),
                    };
                    let suggestion = suggestion.filter(|_| writable && f.value.trim().is_empty());
                    ui.horizontal(|ui| short_edit(ui, f, writable, hint, suggestion, Room { right_edge, widest_long })).inner
                } else {
                    ui.add_enabled(writable, egui::TextEdit::singleline(&mut f.value).desired_width(f32::INFINITY).hint_text(hint))
                };
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

/// Asks for the key of the one open file when its tags have none, once per opening of the file.
fn ask_for_key(app: &mut App, writable: bool) {
    let st = &app.inspector;
    let ([target], KeyDetection::NotAsked) = (st.targets.as_slice(), &st.key_detection) else { return };
    let untagged = st.fields.get(&Field::Key).is_some_and(|f| matches!(&f.original, Original::Same(v) if v.trim().is_empty()));
    if !untagged || !writable {
        return;
    }
    let path = target.clone();
    app.inspector.key_detection = KeyDetection::Asked;
    app.workers.request_key(path);
}

/// Where the short fields' rows end, and how wide the widest long button text of them is.
#[derive(Clone, Copy)]
struct Room {
    right_edge: f32,
    widest_long: f32,
}

fn button_width(ui: &Ui, text: &str) -> f32 {
    // A small button only has less padding; its text is the button text.
    let galley = egui::WidgetText::from(text).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Button);
    galley.size().x + 2.0 * ui.spacing().button_padding.x + 2.0
}

/// A short field with the audio's suggestion for it in a button on the same row. The field keeps
/// its width as long as the long button text fits beside it, else the text shortens, and then the
/// field gives way.
fn short_edit(ui: &mut Ui, f: &mut FieldState, writable: bool, hint: &str, suggestion: Option<&Suggestion>, room: Room) -> egui::Response {
    let spacing = ui.spacing().item_spacing.x;
    let free = (room.right_edge - ui.cursor().left()).max(0.0);
    let (text, width) = match suggestion {
        Some(s) if free >= SHORT_FIELD + spacing + room.widest_long => (Some(&s.long), SHORT_FIELD),
        Some(s) => (Some(&s.short), (free - spacing - button_width(ui, &s.short)).clamp(SHORT_FIELD_MIN, SHORT_FIELD)),
        None => (None, SHORT_FIELD.min(free)),
    };
    let r = ui.add_enabled(writable, egui::TextEdit::singleline(&mut f.value).desired_width(width).hint_text(hint));
    if let (Some(s), Some(text)) = (suggestion, text) {
        let mut button = ui.small_button(text);
        if let Some(tip) = s.tooltip {
            button = button.on_hover_text(tip);
        }
        if button.clicked() {
            f.value = s.value.clone();
            f.touched = match &f.original {
                Original::Same(v) => *v != f.value,
                Original::Mixed => true,
            };
        }
    }
    r
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

/// The artist and title to look a cover up with, as currently typed (so a fix not yet saved counts).
fn cover_query(st: &InspectorState) -> Option<(String, String)> {
    let value = |f: Field| st.fields.get(&f).map(|s| s.value.trim().to_owned()).filter(|v| !v.is_empty());
    value(Field::Artist).zip(value(Field::Title))
}

fn find_cover_online(app: &mut App) {
    let Some((artist, title)) = cover_query(&app.inspector) else { return };
    let request = app.workers.request_id();
    app.inspector.cover_request = Some(request);
    app.workers.request_track_cover(request, artist, title);
}

fn artwork(app: &mut App, ui: &mut Ui) {
    let writable = app.inspector.loaded.values().all(|m| m.writable);
    let searching = app.inspector.cover_request.is_some();
    let find_blocker = if app.inspector.targets.len() != 1 {
        Some("Select a single file to look its cover up")
    } else if !app.settings.online_artwork {
        Some("Online artwork is switched off in the settings")
    } else if cover_query(&app.inspector).is_none() {
        Some("Needs an artist and a title")
    } else {
        None
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new("Artwork").strong());
        if app.inspector.art_touched {
            ui.label(RichText::new("changed").small().color(theme::accent()));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add_enabled(writable, egui::Button::new("Import…").small()).clicked() {
                dialogs::pick_artwork(app);
            }
            let find = egui::Button::new(if searching { "Finding…" } else { "Find online" }).small();
            let hint = find_blocker.unwrap_or("Look up a cover by artist and title (Deezer, then iTunes) and add it to the list, without making it the front cover");
            if ui.add_enabled(writable && !searching && find_blocker.is_none(), find).on_hover_text(hint).on_disabled_hover_text(hint).clicked() {
                find_cover_online(app);
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
    for (i, a) in app.inspector.art.iter().enumerate() {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(96.0, 96.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 3.0, theme::LCD_BG);
            if let Some(t) = &a.texture {
                let s = t.size_vec2();
                let scale = (rect.width() / s.x).min(rect.height() / s.y);
                ui.painter().image(t.id(), egui::Rect::from_center_size(rect.center(), s * scale), egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
            }
            ui.vertical(|ui| {
                ui.label(a.kind.label());
                let dims = a.size.map(|[w, h]| format!("{w}×{h} · ")).unwrap_or_default();
                ui.label(RichText::new(format!("{dims}{} KB", a.data.len() / 1024)).small().color(theme::text_dim()));
                ui.add_space(4.0);
                if a.kind != ArtworkKind::FrontCover && ui.add_enabled(writable, egui::Button::new("Set as front cover").small()).on_hover_text("Tag this image as the front cover, the one other players show as album art").clicked() {
                    make_front = Some(i);
                }
                if ui.add_enabled(writable, egui::Button::new("Remove").small()).clicked() {
                    remove = Some(i);
                }
            });
        });
    }
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
        if let Some(rows) = app.inspector.targets.first().and_then(|p| app.inspector.rekordbox.get(p)).filter(|r| !r.is_empty()) {
            egui::CollapsingHeader::new(RichText::new("From DJ software").small()).id_salt("rekordbox-data").default_open(true).show(ui, |ui| {
                egui::Grid::new("rekordbox-grid").num_columns(2).spacing([10.0, 2.0]).show(ui, |ui| {
                    for (k, v) in rows {
                        ui.label(RichText::new(k).small().color(theme::text_dim()));
                        ui.label(RichText::new(v).small().monospace());
                        ui.end_row();
                    }
                });
            });
        }
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
