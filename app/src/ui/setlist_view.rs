//! The setlist panel: the constituent tracks of the playing DJ mix.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::{Align2, Color32, RichText, Sense, Stroke, Ui, vec2};
use nullamp_core::command::{Command, CommandSource};
use nullamp_core::setlist::{Issue, MixInfo, Provenance, Setlist, SetlistEntry, Timing, Verification};
use nullamp_core::time::{format_clock, format_precise, parse_timestamp};
use nullamp_core::{EntryId, cue, tracklist_text};

use super::{App, dialogs, theme};
use crate::store::SavedSetlist;
use crate::workers::{Job, NowPlayingInfo, OnlineJob, SetlistOrigin};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Cue,
    Text,
}

#[derive(Default)]
pub struct EntryEdit {
    id: Option<EntryId>,
    artist: String,
    title: String,
    version: String,
    label: String,
    start: String,
    end: String,
    exact: bool,
    confirmed: bool,
    error: Option<String>,
}

#[derive(Default)]
pub struct Paste {
    text: String,
    url: String,
    replace: bool,
}

#[derive(Default)]
pub struct MixState {
    pub token: u64,
    pub path: Option<PathBuf>,
    pub fingerprint: Option<String>,
    pub setlist: Option<Setlist>,
    pub origin: Option<SetlistOrigin>,
    pub active: Option<usize>,
    pub selected: Option<EntryId>,
    pub loading: bool,
    pub tracklist_urls: Vec<String>,
    dirty_at: Option<Instant>,
    issues: Vec<Issue>,
    edit: EntryEdit,
    mix_edit: Option<MixInfo>,
    paste: Option<Paste>,
    text_request: Option<u64>,
    scroll_to_active: bool,
    confirm_clear: bool,
    confirm_chapters: bool,
    /// Artwork for setlist entries, by URL (fetched online, cached on disk).
    pub entry_art: std::collections::HashMap<String, egui::TextureHandle>,
    art_requests: std::collections::HashMap<u64, String>,
}

// ----- Lifecycle hooks called from the app -----

pub fn reset_for_track(app: &mut App, token: u64, path: PathBuf) {
    let entry_art = std::mem::take(&mut app.mix.entry_art);
    app.mix = MixState { token, path: Some(path), loading: true, entry_art, ..Default::default() };
}

pub fn on_now_playing(app: &mut App, info: &NowPlayingInfo) {
    if app.mix.token != info.token {
        return;
    }
    // Only the first result after a track change carries the setlist to show; later
    // ones follow a tag write and must not replace what the user sees.
    let refresh = !app.mix.loading;
    app.mix.loading = false;
    app.mix.fingerprint = info.fingerprint.clone();
    app.mix.tracklist_urls = info.tracklist_urls.clone();
    if refresh {
        refresh_tag_entries(app, info.discovered.clone());
        return;
    }
    if let Some((s, origin)) = &info.setlist {
        app.mix.setlist = Some(s.clone());
        app.mix.origin = Some(origin.clone());
        app.mix.scroll_to_active = true;
        revalidate(app);
        if *origin == SetlistOrigin::Discovered {
            let n = s.entries.len();
            let src = s.entries.first().map(|e| e.provenance.label()).unwrap_or_default();
            app.info(format!("Setlist found: {n} tracks from {src}"));
        }
    }
}

/// After the playing file's tags were rewritten: entries read from its Comment or
/// Description follow the tag. Corrections by the user, and entries from any other
/// source (chapters, imports, pasted lists), stay as they are.
fn refresh_tag_entries(app: &mut App, discovered: Option<Setlist>) {
    let from_tag = |e: &SetlistEntry| e.provenance == Provenance::TagDescription && !e.user_edited;
    let key = |e: &SetlistEntry| (e.artist.clone(), e.title.clone(), e.version.clone(), e.label.clone(), e.start);
    let (incoming_mix, incoming): (MixInfo, Vec<SetlistEntry>) = match discovered {
        Some(s) => (s.mix, s.entries.into_iter().filter(|e| e.provenance == Provenance::TagDescription).collect()),
        None => (MixInfo::default(), Vec::new()),
    };
    let current: Vec<_> = app.mix.setlist.iter().flat_map(|s| s.entries.iter()).filter(|e| from_tag(e)).map(key).collect();
    if current == incoming.iter().map(key).collect::<Vec<_>>() {
        return;
    }
    let was = app.mix.origin.clone();
    let mut s = app.mix.setlist.take().unwrap_or_default();
    s.entries.retain(|e| !from_tag(e));
    // A kept correction of an entry the tag still lists must not appear twice.
    let kept: Vec<String> = s.entries.iter().map(|e| format!("{}\u{1f}{}", e.artist.to_lowercase(), e.full_title().to_lowercase())).collect();
    s.entries.extend(incoming.into_iter().filter(|e| !kept.contains(&format!("{}\u{1f}{}", e.artist.to_lowercase(), e.full_title().to_lowercase()))));
    s.sort_by_start();
    s.mix.fill_missing_from(incoming_mix);
    let n = s.entries.iter().filter(|e| from_tag(e)).count();
    app.mix.setlist = (!s.entries.is_empty()).then_some(s);
    if app.mix.selected.is_some_and(|id| !app.mix.setlist.as_ref().is_some_and(|s| s.entries.iter().any(|e| e.id == id))) {
        select(app, None);
    }
    app.mix.scroll_to_active = true;
    if was == Some(SetlistOrigin::Saved) {
        changed(app);
    } else {
        // Nothing of the user's is in it, so it stays a discovery and is not saved.
        revalidate(app);
        app.mix.origin = app.mix.setlist.is_some().then_some(SetlistOrigin::Discovered);
    }
    app.info(if n == 0 { "Setlist from the tags removed".to_owned() } else { format!("Setlist reloaded from the tags: {n} tracks") });
}

/// Track the active entry from the playback position (cheap: runs every frame).
pub fn update_active(app: &mut App) {
    let current_token = app.player.current.as_ref().map(|c| c.token);
    if current_token != Some(app.mix.token) {
        app.mix.active = None;
        return;
    }
    let pos = app.player.position();
    let active = app.mix.setlist.as_ref().and_then(|s| s.active_index(pos));
    if active != app.mix.active {
        app.mix.active = active;
        app.mix.scroll_to_active = true;
        request_entry_art(app);
    }
}

/// Fetch the artwork of the active entry, if it has one and it is not cached yet.
fn request_entry_art(app: &mut App) {
    let Some(url) = app.mix.active.and_then(|i| app.mix.setlist.as_ref()?.entries.get(i)?.artwork_url.clone()) else { return };
    if app.mix.entry_art.contains_key(&url) || app.mix.art_requests.values().any(|u| *u == url) || !app.settings.online_lookups {
        return;
    }
    let request = app.workers.request_id();
    app.mix.art_requests.insert(request, url.clone());
    app.workers.submit_online(OnlineJob::FetchImage { request, hosts: crate::workers::image_hosts_for(&url), url });
}

/// The texture for the active entry's artwork, when loaded.
pub fn active_entry_art(app: &App) -> Option<&egui::TextureHandle> {
    let url = app.mix.active.and_then(|i| app.mix.setlist.as_ref()?.entries.get(i)?.artwork_url.as_ref())?;
    app.mix.entry_art.get(url)
}

pub fn on_entry_art(app: &mut App, request: u64, url: String, result: Result<(std::sync::Arc<[u8]>, crate::workers::Thumb), String>) {
    if app.mix.art_requests.remove(&request).is_none() {
        return;
    }
    match result {
        Ok((_, t)) => {
            let img = egui::ColorImage::from_rgba_unmultiplied(t.size, &t.rgba);
            let tex = app.ctx.load_texture(format!("entry-art-{request}"), img, egui::TextureOptions::LINEAR);
            if app.mix.entry_art.len() > 64 {
                app.mix.entry_art.clear();
            }
            app.mix.entry_art.insert(url, tex);
        }
        Err(e) => log::debug!("entry artwork {url}: {e}"),
    }
}

pub fn apply_lookup_to_entry(app: &mut App, id: EntryId, r: &nullamp_online::RecordingMatch, rel: &nullamp_online::ReleaseRef) {
    if let Some(e) = app.mix.setlist.as_mut().and_then(|s| s.entries.iter_mut().find(|e| e.id == id)) {
        e.release = Some(match &rel.date {
            Some(d) => format!("{} ({d})", rel.title),
            None => rel.title.clone(),
        });
        e.source_url = Some(r.url.clone());
        e.verification = Verification::Confirmed;
        e.user_edited = true;
        if e.artist.trim().is_empty() || e.is_unidentified() {
            e.artist = r.artist.clone();
            e.title = r.title.clone();
        }
    }
    select(app, Some(id));
    changed(app);
    app.info("Release information added to the setlist entry");
}

pub fn set_entry_artwork(app: &mut App, id: EntryId, url: String) {
    if let Some(e) = app.mix.setlist.as_mut().and_then(|s| s.entries.iter_mut().find(|e| e.id == id)) {
        e.artwork_url = Some(url);
        e.user_edited = true;
    }
    changed(app);
    request_entry_art(app);
    app.info("Artwork set for the setlist entry (shown while it plays)");
}

/// Jump to the previous/next setlist entry. "Previous" restarts the current entry
/// first if it has been playing for more than a few seconds.
pub fn jump_relative(app: &mut App, delta: i32) {
    let Some(setlist) = &app.mix.setlist else { return };
    let pos = app.player.position();
    let mut starts: Vec<f64> = setlist.entries.iter().filter_map(|e| e.start).collect();
    starts.sort_by(|a, b| a.total_cmp(b));
    starts.dedup();
    let current = starts.iter().rposition(|s| *s <= pos + 0.05);
    let target = match (delta, current) {
        (d, Some(i)) if d > 0 => starts.get(i + 1).copied(),
        (d, None) if d > 0 => starts.first().copied(),
        (_, Some(i)) if pos - starts[i] > 3.0 => Some(starts[i]),
        (_, Some(i)) if i > 0 => Some(starts[i - 1]),
        _ => Some(0.0),
    };
    if let Some(t) = target {
        app.player.command(Command::SeekTo(t), CommandSource::Keyboard);
    }
}

fn revalidate(app: &mut App) {
    let duration = app.player.duration();
    app.mix.issues = app.mix.setlist.as_ref().map(|s| s.validate(duration)).unwrap_or_default();
}

/// Record a user change: revalidate and schedule a save.
fn changed(app: &mut App) {
    revalidate(app);
    app.mix.origin = Some(SetlistOrigin::Saved);
    app.mix.dirty_at = Some(Instant::now());
}

pub fn flush_setlist_save(app: &mut App, force: bool) {
    let Some(t) = app.mix.dirty_at else { return };
    if !force && t.elapsed() < Duration::from_millis(800) {
        return;
    }
    app.mix.dirty_at = None;
    let (Some(fp), Some(path)) = (app.mix.fingerprint.clone(), app.mix.path.clone()) else {
        app.warn("Setlist changes cannot be saved for this file (it could not be identified)");
        return;
    };
    match &app.mix.setlist {
        Some(s) => app.workers.submit(Job::SaveSetlist(SavedSetlist { fingerprint: fp, last_path: path, setlist: s.clone() })),
        None => app.workers.submit(Job::DeleteSetlist(fp)),
    }
}

/// Bring in a setlist from an import or online source. Existing user corrections are
/// kept (see `Setlist::merge_refresh`).
fn apply_incoming(app: &mut App, incoming: Setlist, replace: bool) {
    let n = incoming.entries.len();
    match app.mix.setlist.as_mut() {
        None => app.mix.setlist = Some(incoming),
        Some(current) if replace => {
            let r = current.merge_refresh(incoming);
            if r.kept_user_edits > 0 {
                app.info(format!("Updated setlist: {} entries, kept {} of your corrections", r.added + r.kept_user_edits, r.kept_user_edits));
            }
        }
        Some(current) => {
            current.entries.extend(incoming.entries);
            current.sort_by_start();
        }
    }
    app.mix.scroll_to_active = true;
    changed(app);
    if n == 0 {
        app.warn("No tracks recognised in that tracklist");
    }
}

pub fn import_file(app: &mut App, path: PathBuf) {
    let request = app.workers.request_id();
    app.mix.text_request = Some(request);
    app.workers.submit(Job::ReadText { request, path });
}

pub fn on_text_loaded(app: &mut App, request: u64, path: PathBuf, text: Result<String, String>) {
    if app.mix.text_request != Some(request) {
        return;
    }
    let text = match text {
        Ok(t) => t,
        Err(e) => return app.error(format!("Cannot read {}: {e}", path.display())),
    };
    let audio_name = app.mix.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let is_cue = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("cue"));
    let incoming = if is_cue {
        let sheet = cue::parse(text.as_bytes());
        match sheet.to_setlist(&audio_name, &path.to_string_lossy()).or_else(|| {
            // A sheet for another file name: use its first file section anyway.
            sheet.files.first().and_then(|f| sheet.to_setlist(&f.name, &path.to_string_lossy()))
        }) {
            Some(s) => s,
            None => return app.warn("That cue sheet has no tracks"),
        }
    } else {
        tracklist_text::parse(&text, None).setlist
    };
    apply_incoming(app, incoming, true);
}

pub fn export_to(app: &mut App, format: ExportFormat, path: PathBuf) {
    let Some(setlist) = &app.mix.setlist else { return };
    let audio_name = app.mix.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let text = match format {
        ExportFormat::Cue => cue::write(setlist, &audio_name),
        ExportFormat::Text => {
            let mut out = String::new();
            if let Some(t) = &setlist.mix.title {
                out.push_str(&format!("{t}\n"));
            }
            if let Some(dj) = &setlist.mix.dj {
                out.push_str(&format!("{dj}\n"));
            }
            out.push('\n');
            for (i, e) in setlist.entries.iter().enumerate() {
                let time = e.start.map(|s| format!("[{}] ", format_clock(s))).unwrap_or_default();
                out.push_str(&format!("{:02}. {time}{} - {}\n", i + 1, e.display_artist(), e.full_title()));
            }
            out
        }
    };
    app.workers.submit(Job::WriteText { path, text });
}

// ----- The panel -----

pub fn show(app: &mut App, ui: &mut Ui) {
    if app.player.current.is_none() {
        ui.add_space(20.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("Nothing playing").color(theme::text_dim()));
            ui.label(RichText::new("Play a DJ mix to see its setlist here.").small().color(theme::text_faint()));
        });
        return;
    }
    mix_card(app, ui);
    ui.add_space(4.0);
    toolbar(app, ui);
    ui.add_space(4.0);
    if app.mix.loading {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new("Looking for a setlist…").color(theme::text_dim()));
        });
        return;
    }
    if app.mix.setlist.as_ref().is_none_or(|s| s.entries.is_empty()) {
        empty_hint(app, ui);
    } else {
        source_summary(app, ui);
        entries(app, ui);
        ui.add_space(4.0);
        editor(app, ui);
    }
    paste_modal(app, ui.ctx());
    confirm_modals(app, ui.ctx());
}

fn mix_card(app: &mut App, ui: &mut Ui) {
    let entry_title = app.player.current_entry().map(|e| e.info.display_title(&e.path)).unwrap_or_default();
    let entry_artist = app.player.current_entry().and_then(|e| e.info.artist.clone());
    egui::Frame::new().fill(theme::raised()).corner_radius(4.0).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        if let Some(edit) = app.mix.mix_edit.as_mut() {
            let mut done = None;
            egui::Grid::new("mix-edit").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                for (label, value) in [("Mix title", &mut edit.title), ("DJ", &mut edit.dj), ("Event", &mut edit.event), ("Date", &mut edit.date), ("URL", &mut edit.url)] {
                    ui.label(RichText::new(label).color(theme::text_dim()));
                    let mut s = value.clone().unwrap_or_default();
                    if ui.add(egui::TextEdit::singleline(&mut s).desired_width(f32::INFINITY)).changed() {
                        *value = Some(s).filter(|s| !s.trim().is_empty());
                    }
                    ui.end_row();
                }
            });
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    done = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    done = Some(false);
                }
            });
            match done {
                Some(true) => {
                    let mix = app.mix.mix_edit.take().unwrap();
                    app.mix.setlist.get_or_insert_with(Setlist::default).mix = mix;
                    changed(app);
                }
                Some(false) => app.mix.mix_edit = None,
                None => {}
            }
            return;
        }
        let mix = app.mix.setlist.as_ref().map(|s| s.mix.clone()).unwrap_or_default();
        ui.horizontal(|ui| {
            ui.label(RichText::new("MIX").size(10.0).color(theme::mix_tag()).monospace());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("✎").on_hover_text("Edit mix information (does not touch the tracks)").clicked() {
                    let mut m = mix.clone();
                    if m.title.is_none() {
                        m.title = Some(entry_title.clone());
                    }
                    if m.dj.is_none() {
                        m.dj = entry_artist.clone();
                    }
                    app.mix.mix_edit = Some(m);
                }
            });
        });
        ui.add(egui::Label::new(RichText::new(mix.title.clone().unwrap_or(entry_title)).strong().size(14.0)).truncate());
        let mut sub = Vec::new();
        if let Some(dj) = mix.dj.clone().or(entry_artist) {
            sub.push(dj);
        }
        if let Some(e) = &mix.event {
            sub.push(e.clone());
        }
        if let Some(d) = &mix.date {
            sub.push(d.clone());
        }
        if !sub.is_empty() {
            ui.label(RichText::new(sub.join(" · ")).color(theme::text_dim()));
        }
        if let Some(url) = &mix.url {
            ui.hyperlink_to(RichText::new(url).small(), url);
        }
        // Currently playing constituent track, visually distinct from the mix identity.
        if let Some((s, i)) = app.mix.setlist.as_ref().zip(app.mix.active) {
            let e = &s.entries[i];
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("NOW").size(10.0).monospace().color(theme::now()));
                ui.add(egui::Label::new(RichText::new(super::deck::entry_line(e)).color(theme::now())).truncate());
            });
        }
    });
}

fn toolbar(app: &mut App, ui: &mut Ui) {
    ui.horizontal_wrapped(|ui| {
        ui.menu_button("Import", |ui| {
            if ui.button("Cue sheet or text file…").clicked() {
                dialogs::import_setlist(app);
                ui.close();
            }
            if ui.button("Paste tracklist…").on_hover_text("Paste a tracklist copied from a web page such as 1001Tracklists").clicked() {
                app.mix.paste = Some(Paste { replace: true, ..Default::default() });
                ui.close();
            }
            if ui.button("Scan file again").on_hover_text("Re-read chapters and cue sheets; your corrections are kept").clicked() {
                rescan(app);
                ui.close();
            }
        });
        let has = app.mix.setlist.as_ref().is_some_and(|s| !s.entries.is_empty());
        ui.add_enabled_ui(has, |ui| {
            ui.menu_button("Export", |ui| {
                if ui.button("Cue sheet…").clicked() {
                    dialogs::export_setlist(app, ExportFormat::Cue);
                    ui.close();
                }
                if ui.button("Text tracklist…").clicked() {
                    dialogs::export_setlist(app, ExportFormat::Text);
                    ui.close();
                }
                let is_mp3 = app.mix.path.as_ref().is_some_and(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("mp3")));
                if ui.add_enabled(is_mp3, egui::Button::new("Write as MP3 chapters…")).on_hover_text("Store the setlist inside the MP3 file (ID3 CHAP frames)").clicked() {
                    app.mix.confirm_chapters = true;
                    ui.close();
                }
                ui.separator();
                if ui.button("Copy as text").clicked() {
                    if let Some(s) = &app.mix.setlist {
                        let text: String = s
                            .entries
                            .iter()
                            .map(|e| format!("{}{} - {}\n", e.start.map(|t| format!("[{}] ", format_clock(t))).unwrap_or_default(), e.display_artist(), e.full_title()))
                            .collect();
                        ui.ctx().copy_text(text);
                    }
                    ui.close();
                }
            });
        });
        if ui.button("+ At playhead").on_hover_text("Add an entry starting at the current position").clicked() {
            let pos = app.player.position();
            let mut e = SetlistEntry::new("", "", Some((pos * 10.0).round() / 10.0), Provenance::Manual);
            e.timing = Timing::Exact;
            e.user_edited = true;
            let id = e.id;
            let s = app.mix.setlist.get_or_insert_with(Setlist::default);
            s.entries.push(e);
            s.sort_by_start();
            select(app, Some(id));
            changed(app);
        }
        if has && ui.button("Clear…").clicked() {
            app.mix.confirm_clear = true;
        }
    });
}

fn rescan(app: &mut App) {
    let Some(path) = app.mix.path.clone() else { return };
    let request = app.workers.request_id();
    app.mix.text_request = Some(request);
    app.workers.submit(Job::Rediscover { request, path });
}

pub fn on_rediscovered(app: &mut App, request: u64, setlist: Option<Setlist>) {
    if app.mix.text_request != Some(request) {
        return;
    }
    match setlist {
        Some(s) => apply_incoming(app, s, true),
        None => app.info("No chapters or cue sheet found for this file"),
    }
}

fn empty_hint(app: &mut App, ui: &mut Ui) {
    ui.add_space(6.0);
    ui.label(RichText::new("No setlist for this file.").color(theme::text_dim()));
    ui.label(
        RichText::new("Nullamp looked for embedded chapters, a cue sheet in the tags, and a .cue file next to it. Import a cue sheet, paste a tracklist copied from a web page, or add entries at the playhead.")
            .small()
            .color(theme::text_faint()),
    );
    tracklist_links(app, ui);
}

fn tracklist_links(app: &mut App, ui: &mut Ui) {
    let urls = app.mix.tracklist_urls.clone();
    for url in urls {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Tracklist link in tags:").small().color(theme::text_dim()));
            ui.hyperlink_to(RichText::new(&url).small(), &url);
            if url.contains("1001tracklists.com") && ui.small_button("Paste from it…").on_hover_text("1001Tracklists has no API: open the page, copy the list, paste it here").clicked() {
                app.mix.paste = Some(Paste { url: url.clone(), replace: true, ..Default::default() });
            }
        });
    }
}

fn source_summary(app: &mut App, ui: &mut Ui) {
    let Some(s) = &app.mix.setlist else { return };
    let mut sources: Vec<String> = Vec::new();
    for e in &s.entries {
        let l = e.provenance.label();
        if !sources.contains(&l) {
            sources.push(l);
        }
    }
    let untimed = s.entries.iter().all(|e| e.start.is_none());
    let approx = s.entries.iter().filter(|e| e.start.is_some() && e.timing == Timing::Approximate).count();
    let edited = s.entries.iter().filter(|e| e.user_edited).count();
    let saved = app.mix.origin == Some(SetlistOrigin::Saved);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.label(RichText::new(format!("{} tracks", s.entries.len())).small().color(theme::text()));
        ui.label(RichText::new(format!("· {}", sources.join(", "))).small().color(theme::text_dim()));
        if untimed {
            ui.label(RichText::new("· no times, reference only").small().color(theme::text_dim()));
        } else if approx > 0 {
            ui.label(RichText::new(format!("· ≈ {approx} approximate")).small().color(theme::warn()))
                .on_hover_text("These start times are estimates from the source; boundaries are drawn dashed on the timeline.");
        } else {
            ui.label(RichText::new("· exact timing").small().color(theme::ok()));
        }
        if edited > 0 {
            ui.label(RichText::new(format!("· ✎ {edited} corrected")).small().color(theme::text_dim()));
        }
        if saved {
            ui.label(RichText::new("· saved locally").small().color(theme::text_faint()));
        }
    });
    let issues = app.mix.issues.clone();
    if !issues.is_empty() {
        egui::CollapsingHeader::new(RichText::new(format!("⚠ {} problem{}", issues.len(), if issues.len() == 1 { "" } else { "s" })).small().color(theme::warn()))
            .id_salt("setlist-issues")
            .show(ui, |ui| {
                for issue in issues.iter().take(30) {
                    if ui.add(egui::Label::new(RichText::new(issue.describe()).small()).sense(Sense::click())).clicked() {
                        let id = app.mix.setlist.as_ref().and_then(|s| s.entries.get(issue.index()).map(|e| e.id));
                        select(app, id);
                    }
                }
            });
    }
    tracklist_links(app, ui);
}

fn select(app: &mut App, id: Option<EntryId>) {
    app.mix.selected = id;
    let Some(e) = id.and_then(|id| app.mix.setlist.as_ref()?.entries.iter().find(|e| e.id == id)) else {
        app.mix.edit = EntryEdit::default();
        return;
    };
    app.mix.edit = EntryEdit {
        id: Some(e.id),
        artist: e.artist.clone(),
        title: e.title.clone(),
        version: e.version.clone().unwrap_or_default(),
        label: e.label.clone().unwrap_or_default(),
        start: e.start.map(format_precise).unwrap_or_default(),
        end: e.end.map(format_precise).unwrap_or_default(),
        exact: e.timing == Timing::Exact,
        confirmed: e.verification == Verification::Confirmed,
        error: None,
    };
}

fn entries(app: &mut App, ui: &mut Ui) {
    let Some(setlist) = app.mix.setlist.clone() else { return };
    let max_h = (ui.available_height() * 0.62).max(160.0);
    let scroll_to = std::mem::take(&mut app.mix.scroll_to_active);
    let mut clicked = None;
    let mut seek = None;
    let mut menu_action: Option<(EntryId, &'static str)> = None;
    egui::ScrollArea::vertical().id_salt("setlist-entries").max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for (i, e) in setlist.entries.iter().enumerate() {
            let active = app.mix.active == Some(i);
            let selected = app.mix.selected == Some(e.id);
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
            let p = ui.painter();
            let bg = if selected { theme::select_bg() } else if resp.hovered() { theme::raised() } else if i % 2 == 1 { theme::row_alt() } else { theme::panel() };
            p.rect_filled(rect, 0.0, bg);
            if active {
                p.rect_filled(egui::Rect::from_min_size(rect.left_top(), vec2(3.0, rect.height())), 0.0, theme::now());
            }
            let time_text = match e.start {
                Some(s) if e.timing == Timing::Approximate => format!("≈{}", format_clock(s)),
                Some(s) => format_clock(s),
                None if e.provenance == Provenance::TagDescription => String::new(),
                None => "--:--".into(),
            };
            let time_color = if e.start.is_none() { theme::warn() } else if active { theme::now() } else { theme::text_dim() };
            p.text(rect.left_top() + vec2(10.0, 9.0), Align2::LEFT_CENTER, format!("{:>2}", i + 1), theme::mono(10.0), theme::text_faint());
            p.text(rect.left_top() + vec2(10.0, 24.0), Align2::LEFT_CENTER, time_text, theme::mono(10.5), time_color);
            let text_left = rect.left() + 70.0;
            let width = rect.right() - text_left - 24.0;
            let title_color = if active { theme::now() } else if e.is_unidentified() { theme::text_faint() } else { theme::text() };
            clip_text(ui, egui::pos2(text_left, rect.top() + 9.0), width, &e.full_title(), theme::prop(12.5), title_color);
            let mut sub = e.display_artist().to_owned();
            if let Some(l) = &e.label {
                sub.push_str(&format!("  [{l}]"));
            }
            clip_text(ui, egui::pos2(text_left, rect.top() + 24.0), width, &sub, theme::prop(11.0), theme::text_dim());
            let mark = if e.user_edited {
                Some(("✎", theme::text_dim(), "You corrected this entry"))
            } else if e.verification == Verification::Confirmed {
                Some(("✓", theme::ok(), "Confirmed"))
            } else if e.is_unidentified() {
                Some(("?", theme::text_faint(), "Unidentified track"))
            } else {
                None
            };
            if let Some((m, c, _)) = mark {
                p.text(egui::pos2(rect.right() - 10.0, rect.center().y), Align2::RIGHT_CENTER, m, theme::prop(12.0), c);
            }
            p.line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, Color32::from_black_alpha(if theme::is_light() { 18 } else { 60 })));
            if active && scroll_to {
                ui.scroll_to_rect(rect, Some(egui::Align::Center));
            }
            let hover = format!(
                "{}\nSource: {}{}",
                e.start.map(|s| format!("Starts at {}", format_precise(s))).unwrap_or_else(|| "No start time".into()),
                e.provenance.label(),
                mark.map(|(_, _, t)| format!("\n{t}")).unwrap_or_default()
            );
            let resp = resp.on_hover_text(hover);
            if resp.clicked() {
                clicked = Some(e.id);
            }
            if resp.double_clicked() {
                seek = e.start;
            }
            resp.context_menu(|ui| {
                if ui.add_enabled(e.start.is_some(), egui::Button::new("Jump here")).clicked() {
                    seek = e.start;
                    ui.close();
                }
                if ui.button("Set start to playhead").clicked() {
                    menu_action = Some((e.id, "set-start"));
                    ui.close();
                }
                if ui.button("Delete entry").clicked() {
                    menu_action = Some((e.id, "delete"));
                    ui.close();
                }
            });
        }
    });
    if let Some(id) = clicked {
        select(app, Some(id));
    }
    if let Some(t) = seek {
        app.player.command(Command::SeekTo(t), CommandSource::Ui);
    }
    match menu_action {
        Some((id, "set-start")) => {
            let pos = app.player.position();
            if let Some(e) = app.mix.setlist.as_mut().and_then(|s| s.entries.iter_mut().find(|e| e.id == id)) {
                e.start = Some((pos * 10.0).round() / 10.0);
                e.timing = Timing::Exact;
                e.user_edited = true;
            }
            if let Some(s) = app.mix.setlist.as_mut() {
                s.sort_by_start();
            }
            select(app, Some(id));
            changed(app);
        }
        Some((id, "delete")) => delete_entry(app, id),
        _ => {}
    }
}

fn clip_text(ui: &Ui, pos: egui::Pos2, width: f32, text: &str, font: egui::FontId, color: Color32) {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(width.max(1.0));
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    ui.painter().galley(egui::pos2(pos.x, pos.y - galley.size().y / 2.0), galley, color);
}

fn delete_entry(app: &mut App, id: EntryId) {
    if let Some(s) = app.mix.setlist.as_mut() {
        s.entries.retain(|e| e.id != id);
    }
    if app.mix.selected == Some(id) {
        select(app, None);
    }
    changed(app);
}

fn editor(app: &mut App, ui: &mut Ui) {
    let Some(id) = app.mix.edit.id else {
        ui.label(RichText::new("Select an entry to edit it. Double-click to jump.").small().color(theme::text_faint()));
        return;
    };
    let pos = app.player.position();
    let mut apply = false;
    let mut delete = false;
    let mut lookup = false;
    let mut moved: Option<i32> = None;
    egui::Frame::new().fill(theme::raised()).corner_radius(4.0).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
        let ed = &mut app.mix.edit;
        egui::Grid::new("entry-edit").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
            let row = |ui: &mut Ui, label: &str, value: &mut String| {
                ui.label(RichText::new(label).color(theme::text_dim()));
                ui.add(egui::TextEdit::singleline(value).desired_width(f32::INFINITY));
                ui.end_row();
            };
            row(ui, "Artist", &mut ed.artist);
            row(ui, "Title", &mut ed.title);
            row(ui, "Version", &mut ed.version);
            row(ui, "Label", &mut ed.label);
            ui.label(RichText::new("Start").color(theme::text_dim()));
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut ed.start).desired_width(90.0).hint_text("m:ss"));
                if ui.small_button("→ playhead").on_hover_text(format!("Set to {}", format_precise(pos))).clicked() {
                    ed.start = format_precise(pos);
                    ed.exact = true;
                }
                ui.checkbox(&mut ed.exact, "exact").on_hover_text("Unchecked: the time is an estimate (shown with ≈)");
            });
            ui.end_row();
            ui.label(RichText::new("End").color(theme::text_dim()));
            ui.add(egui::TextEdit::singleline(&mut ed.end).desired_width(90.0).hint_text("optional"));
            ui.end_row();
            ui.label("");
            ui.checkbox(&mut ed.confirmed, "Track identity confirmed");
            ui.end_row();
        });
        if let Some(err) = &ed.error {
            ui.label(RichText::new(err).small().color(theme::err()));
        }
        if let Some(e) = app.mix.setlist.as_ref().and_then(|s| s.entries.iter().find(|e| e.id == id)) {
            if let Some(r) = &e.release {
                ui.label(RichText::new(format!("Release: {r}")).small().color(theme::text_dim()));
            }
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Source: {}", e.provenance.label())).small().color(theme::text_faint()));
                if let Some(url) = e.provenance.url().or(e.source_url.as_deref()) {
                    ui.hyperlink_to(RichText::new("open").small(), url);
                }
            });
        }
        ui.horizontal(|ui| {
            if ui.button("Apply").clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)) && ui.ctx().egui_wants_keyboard_input() {
                apply = true;
            }
            if ui.small_button("Look up…").on_hover_text("Find release info and artwork on MusicBrainz").clicked() {
                lookup = true;
            }
            if ui.small_button("▲").clicked() {
                moved = Some(-1);
            }
            if ui.small_button("▼").clicked() {
                moved = Some(1);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(RichText::new("Delete").color(theme::err())).clicked() {
                    delete = true;
                }
            });
        });
    });
    if apply {
        apply_edit(app, id);
    }
    if lookup {
        let (artist, title) = (app.mix.edit.artist.clone(), app.mix.edit.title.clone());
        super::inspector::lookup_for_entry(app, id, artist, title);
    }
    if delete {
        delete_entry(app, id);
    }
    if let Some(d) = moved
        && let Some(s) = app.mix.setlist.as_mut()
        && let Some(i) = s.entries.iter().position(|e| e.id == id)
    {
        let j = (i as i32 + d).clamp(0, s.entries.len() as i32 - 1) as usize;
        s.entries.swap(i, j);
        if let Some(e) = s.entries.get_mut(j) {
            e.user_edited = true;
        }
        changed(app);
    }
}

fn apply_edit(app: &mut App, id: EntryId) {
    let ed = &app.mix.edit;
    let parse = |s: &str| -> Result<Option<f64>, String> {
        if s.trim().is_empty() {
            Ok(None)
        } else {
            parse_timestamp(s).map(Some).ok_or_else(|| format!("“{s}” is not a time (use m:ss or h:mm:ss)"))
        }
    };
    let (start, end) = match (parse(&ed.start), parse(&ed.end)) {
        (Ok(s), Ok(e)) => (s, e),
        (Err(e), _) | (_, Err(e)) => {
            app.mix.edit.error = Some(e);
            return;
        }
    };
    let opt = |s: &str| Some(s.trim().to_owned()).filter(|s| !s.is_empty());
    let (artist, title, version, label, exact, confirmed) =
        (ed.artist.trim().to_owned(), ed.title.trim().to_owned(), opt(&ed.version), opt(&ed.label), ed.exact, ed.confirmed);
    if let Some(e) = app.mix.setlist.as_mut().and_then(|s| s.entries.iter_mut().find(|e| e.id == id)) {
        e.artist = artist;
        e.title = title;
        e.version = version;
        e.label = label;
        e.start = start;
        e.end = end;
        e.timing = if exact { Timing::Exact } else { Timing::Approximate };
        e.verification = if confirmed {
            Verification::Confirmed
        } else if e.is_unidentified() {
            Verification::Unidentified
        } else {
            Verification::Suggested
        };
        e.user_edited = true;
    }
    if let Some(s) = app.mix.setlist.as_mut() {
        s.sort_by_start();
    }
    app.mix.edit.error = None;
    changed(app);
}

fn paste_modal(app: &mut App, ctx: &egui::Context) {
    let Some(mut paste) = app.mix.paste.take() else { return };
    let mut keep = true;
    egui::Modal::new(egui::Id::unique("paste-tracklist")).show(ctx, |ui| {
        ui.set_width(560.0);
        ui.heading("Paste tracklist");
        ui.label(
            RichText::new("Copy a tracklist from a web page (1001Tracklists, Mixcloud, a forum post…) and paste it below. Lines like “[12:30] Artist - Title (Remix)” are recognised; timestamps are treated as approximate.")
                .small()
                .color(theme::text_dim()),
        );
        egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
            ui.add(egui::TextEdit::multiline(&mut paste.text).desired_rows(12).desired_width(f32::INFINITY).font(egui::TextStyle::Monospace));
        });
        ui.horizontal(|ui| {
            ui.label("Source page");
            ui.add(egui::TextEdit::singleline(&mut paste.url).desired_width(f32::INFINITY).hint_text("https://… (optional, kept as provenance)"));
        });
        let url = Some(paste.url.trim().to_owned()).filter(|u| !u.is_empty());
        let parsed = tracklist_text::parse(&paste.text, url);
        let timed = parsed.setlist.entries.iter().filter(|e| e.start.is_some()).count();
        ui.label(
            RichText::new(format!("{} tracks recognised ({timed} with times), {} lines ignored", parsed.setlist.entries.len(), parsed.ignored.len()))
                .small()
                .color(if parsed.setlist.entries.is_empty() { theme::warn() } else { theme::ok() }),
        );
        if app.mix.setlist.as_ref().is_some_and(|s| !s.entries.is_empty()) {
            ui.checkbox(&mut paste.replace, "Replace current entries (your corrections are kept)");
        }
        ui.horizontal(|ui| {
            if ui.add_enabled(!parsed.setlist.entries.is_empty(), egui::Button::new("Import")).clicked() {
                apply_incoming(app, parsed.setlist.clone(), paste.replace);
                keep = false;
            }
            if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                keep = false;
            }
        });
    });
    if keep {
        app.mix.paste = Some(paste);
    }
}

fn confirm_modals(app: &mut App, ctx: &egui::Context) {
    if app.mix.confirm_clear {
        egui::Modal::new(egui::Id::unique("clear-setlist")).show(ctx, |ui| {
            ui.heading("Clear the setlist?");
            ui.label("All entries for this file are removed, including your corrections.");
            ui.horizontal(|ui| {
                if ui.button(RichText::new("Clear").color(theme::err())).clicked() {
                    app.mix.setlist = None;
                    app.mix.confirm_clear = false;
                    select(app, None);
                    changed(app);
                }
                if ui.button("Cancel").clicked() {
                    app.mix.confirm_clear = false;
                }
            });
        });
    }
    if app.mix.confirm_chapters {
        egui::Modal::new(egui::Id::unique("write-chapters")).show(ctx, |ui| {
            ui.heading("Write setlist into the MP3?");
            ui.label("Existing chapters in the file are replaced by this setlist. Other tags and the audio are preserved. A backup copy is kept if backups are enabled.");
            ui.horizontal(|ui| {
                if ui.button("Write chapters").clicked() {
                    app.mix.confirm_chapters = false;
                    super::inspector::write_chapters(app);
                }
                if ui.button("Cancel").clicked() {
                    app.mix.confirm_chapters = false;
                }
            });
        });
    }
}
