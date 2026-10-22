//! The setlist panel: the constituent tracks of the playing DJ mix.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::{Align2, Color32, RichText, Sense, Stroke, Ui, vec2};
use nullamp_core::command::{Command, CommandSource};
use nullamp_core::setlist::{Issue, MixInfo, Provenance, Setlist, SetlistEntry, Timing, Verification};
use nullamp_core::time::{format_clock, format_precise, parse_timestamp};
use nullamp_core::{EntryId, cue, tracklist_text};
use nullamp_shazam::{Song, Verdict};

use super::{App, dialogs, theme};
use crate::store::SavedSetlist;
use crate::workers::{IdentifyTrack, Job, NowPlayingInfo, SetlistOrigin};

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
    /// Whether the playing file counts as a DJ mix, so the setlist parts of the UI show.
    /// Kept from the previous file until the new one's setlist and duration are known,
    /// so the panels do not flash on a track change.
    pub is_mix: bool,
    /// The user asked for the setlist panel on a file that does not look like a mix.
    pub pinned: bool,
    dirty_at: Option<Instant>,
    issues: Vec<Issue>,
    edit: EntryEdit,
    paste: Option<Paste>,
    text_request: Option<u64>,
    scroll_to_active: bool,
    confirm_clear: bool,
    confirm_chapters: bool,
    confirm_cue_comment: bool,
    identify: Option<Identifying>,
    /// Songs Shazam named for entries it was unsure about, or that were open in the editor
    /// when the answer came, for the user to pick from in the editor. Not saved.
    candidates: HashMap<EntryId, Vec<Song>>,
    split: Option<Splitting>,
}

/// A running "Find tracks" request.
struct Splitting {
    request: u64,
    /// The untimed entries the starts are for, 0 without a list (see [`untimed_count`]).
    listed: usize,
}

/// A running "Identify with Shazam" request.
struct Identifying {
    request: u64,
    entry: EntryId,
    /// Clips asked about so far, of `total`.
    done: usize,
    total: usize,
    /// What to say when the request is done.
    outcome: Option<String>,
}

// ----- Lifecycle hooks called from the app -----

pub fn reset_for_track(app: &mut App, token: u64, path: PathBuf) {
    // The setlist shown is about to change; its tracks are no longer worth asking about.
    if app.mix.identify.is_some() {
        app.workers.cancel_identify();
    }
    if app.mix.split.is_some() {
        app.workers.cancel_split();
    }
    app.mix = MixState { token, path: Some(path), loading: true, is_mix: app.mix.is_mix, ..Default::default() };
}

/// Files at least this long are treated as mixes even before they have a setlist;
/// above nearly all single tracks, well below real DJ mixes.
const MIX_MIN_SECONDS: f64 = 20.0 * 60.0;

/// A file is a mix when it has a setlist, or failing that, when it is long. Single
/// tracks rarely carry one: discovery wants two chapters or cue tracks, three
/// description lines, and a single entry only counts when the user made it.
fn detect_mix(app: &App) -> bool {
    let m = &app.mix;
    if app.player.current.is_none() {
        return false;
    }
    if m.pinned || m.setlist.as_ref().is_some_and(|s| s.entries.len() >= 2 || (!s.entries.is_empty() && m.origin == Some(SetlistOrigin::Saved))) {
        return true;
    }
    match app.player.duration().filter(|d| *d > 0.0) {
        Some(d) if !m.loading => d >= MIX_MIN_SECONDS,
        _ => m.is_mix,
    }
}

/// The setlist of the playing file, when the UI shows it.
pub fn shown(app: &App) -> Option<&Setlist> {
    app.mix.setlist.as_ref().filter(|_| app.mix.is_mix)
}

/// Open the setlist panel on a file that does not look like a mix, to add one.
pub fn pin(app: &mut App) {
    app.mix.pinned = true;
    app.mix.is_mix = true;
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
/// Description (a track list or a cue sheet) follow the tag. Corrections by the user, and entries from any other
/// source (chapters, imports, pasted lists), stay as they are.
fn refresh_tag_entries(app: &mut App, discovered: Option<Setlist>) {
    let from_tag = |e: &SetlistEntry| e.provenance.is_from_tags() && !e.user_edited;
    let key = |e: &SetlistEntry| (e.artist.clone(), e.title.clone(), e.version.clone(), e.label.clone(), e.start);
    let (incoming_mix, mut incoming): (MixInfo, Vec<SetlistEntry>) = match discovered {
        Some(s) => (s.mix, s.entries.into_iter().filter(|e| e.provenance.is_from_tags()).collect()),
        None => (MixInfo::default(), Vec::new()),
    };
    keep_found_times(app.mix.setlist.as_ref().map_or(&[][..], |s| &s.entries[..]), &mut incoming);
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

/// Times placed on the tag's track list (by "Find tracks") stay when the tag is rewritten: by
/// position when it still lists as many tracks (so fixing a name keeps its time), else by name.
fn keep_found_times(current: &[SetlistEntry], incoming: &mut [SetlistEntry]) {
    let timed: Vec<&SetlistEntry> = current.iter().filter(|e| e.provenance == Provenance::TagDescription && !e.user_edited).collect();
    if timed.iter().all(|e| e.start.is_none()) || incoming.iter().any(|e| e.start.is_some()) {
        return;
    }
    let same = |a: &SetlistEntry, b: &SetlistEntry| a.artist == b.artist && a.full_title() == b.full_title();
    let by_position = timed.len() == incoming.len();
    let mut used = vec![false; timed.len()];
    for (i, e) in incoming.iter_mut().enumerate() {
        let from = if by_position { Some(i) } else { (0..timed.len()).find(|&k| !used[k] && same(timed[k], e)) };
        if let Some(k) = from {
            used[k] = true;
            (e.start, e.timing) = (timed[k].start, timed[k].timing);
        }
    }
}

/// Track the active entry from the playback position (cheap: runs every frame).
pub fn update_active(app: &mut App) {
    app.mix.is_mix = detect_mix(app);
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
    }
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
    let is_cue = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("cue")) || cue::looks_like_cue(&text);
    let incoming = if is_cue {
        match cue_setlist(&text, &audio_name, &path.to_string_lossy()) {
            Some(s) => s,
            None => return app.warn("That cue sheet has no tracks"),
        }
    } else {
        tracklist_text::parse(&text, None).setlist
    };
    apply_incoming(app, incoming, true);
}

fn cue_setlist(text: &str, audio_name: &str, origin: &str) -> Option<Setlist> {
    let sheet = cue::parse(text.as_bytes());
    sheet.to_setlist(audio_name, origin).or_else(|| {
        // A sheet for another file name: use its first file section anyway.
        sheet.files.first().and_then(|f| sheet.to_setlist(&f.name, origin))
    })
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
        let mix = app.mix.setlist.as_ref().map(|s| s.mix.clone()).unwrap_or_default();
        ui.label(RichText::new("MIX").size(10.0).color(theme::mix_tag()).monospace());
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
                let untimed = app.mix.setlist.as_ref().map_or(0, |s| s.entries.iter().filter(|e| e.start.is_none()).count());
                let hover = if untimed == 0 {
                    "Replace the file's comment with a cue sheet of this setlist, for a list you have finished correcting".to_owned()
                } else {
                    format!("{untimed} {} no start time yet; a cue sheet needs one for every track", if untimed == 1 { "entry has" } else { "entries have" })
                };
                if ui.add_enabled(untimed == 0, egui::Button::new("Write cue sheet into the comment…")).on_hover_text(hover.clone()).on_disabled_hover_text(hover).clicked() {
                    app.mix.confirm_cue_comment = true;
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
        find_tracks_button(app, ui);
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

// ----- Identify with Shazam (one entry at a time, from its right-click menu) -----

/// Whether Shazam's answer may replace the entry's name: the user has not typed its artist or
/// title (a marker placed with "+ At playhead" counts as untyped until it has a name).
fn fillable(e: &SetlistEntry) -> bool {
    !e.user_edited || e.is_unidentified()
}

/// Where entry `id` plays: from its start to where the next entry starts (or the end of the file).
fn identify_span(app: &App, id: EntryId) -> Option<IdentifyTrack> {
    let s = app.mix.setlist.as_ref()?;
    let timed: Vec<&SetlistEntry> = s.entries.iter().filter(|e| e.start.is_some()).collect();
    let i = timed.iter().position(|e| e.id == id)?;
    let start = timed[i].start?;
    let length = app.player.duration().filter(|d| *d > 0.0);
    let end = timed.get(i + 1).and_then(|n| n.start).or(timed[i].end).or(length).unwrap_or(start + 600.0);
    (end > start).then_some(IdentifyTrack { entry: id, start, end })
}

/// Ask Shazam about one entry. Its answer replaces the name only of an entry the user has not
/// named; otherwise it is offered in the editor.
fn identify_one(app: &mut App, id: EntryId) {
    let (Some(path), Some(track)) = (app.mix.path.clone(), identify_span(app, id)) else { return };
    let request = app.workers.request_id();
    app.workers.identify(request, path, vec![track]);
    app.mix.identify = Some(Identifying { request, entry: id, done: 0, total: nullamp_shazam::CLIPS_PER_TRACK, outcome: None });
}

fn cancel_identify(app: &mut App) {
    app.workers.cancel_identify();
    app.mix.identify = None;
}

fn identifying(app: &mut App, request: u64) -> Option<&mut Identifying> {
    app.mix.identify.as_mut().filter(|i| i.request == request)
}

pub fn on_identify_progress(app: &mut App, request: u64, done: usize, total: usize) {
    if let Some(i) = identifying(app, request) {
        (i.done, i.total) = (done, total);
    }
}

/// A named song fills its entry as a suggestion, unless the user has named the entry or has it
/// open in the editor; then, like an unsure answer, it is only offered there.
pub fn on_identified(app: &mut App, request: u64, entry: EntryId, verdict: Verdict) {
    if identifying(app, request).is_none() {
        return;
    }
    let editing = app.mix.edit.id == Some(entry);
    let (offer, outcome, filled) = match verdict {
        Verdict::Named { song, .. } => {
            let Some(e) = app.mix.setlist.as_mut().and_then(|s| s.entries.iter_mut().find(|e| e.id == entry)) else { return };
            let name = format!("{} - {}", song.artist, song.title);
            if !fillable(e) || editing {
                (vec![song], format!("Shazam named it {name}; edit the entry to use that"), false)
            } else {
                e.artist = song.artist;
                e.title = song.title;
                e.version = None;
                e.verification = Verification::Suggested;
                e.provenance = Provenance::Online { provider: "Shazam".into(), url: song.url };
                (Vec::new(), format!("Shazam named the track {name}"), true)
            }
        }
        Verdict::Unsure { candidates } if candidates.is_empty() => (Vec::new(), "Shazam does not know the track".to_owned(), false),
        Verdict::Unsure { candidates } => (candidates, "Shazam is unsure about the track; edit it to pick one of its answers".to_owned(), false),
    };
    if offer.is_empty() {
        app.mix.candidates.remove(&entry);
    } else {
        app.mix.candidates.insert(entry, offer);
    }
    if let Some(i) = identifying(app, request) {
        i.outcome = Some(outcome);
    }
    if filled {
        changed(app);
    }
}

pub fn on_identify_done(app: &mut App, request: u64, result: Result<(), String>) {
    let Some(i) = identifying(app, request) else { return };
    let outcome = i.outcome.take();
    app.mix.identify = None;
    match result {
        Ok(()) => app.info(outcome.unwrap_or_else(|| "Shazam did not answer".into())),
        Err(e) => app.warn(e),
    }
}

// ----- Find tracks (offline, from the audio) -----

/// How many entries "Find tracks" would give start times: those of a list without any times yet,
/// 0 without a list. `None` once the list has times, so they are never overwritten.
fn untimed_count(app: &App) -> Option<usize> {
    match &app.mix.setlist {
        Some(s) if s.has_times() => None,
        Some(s) => Some(s.entries.len()),
        None => Some(0),
    }
}

fn find_tracks_button(app: &mut App, ui: &mut Ui) {
    if app.mix.split.is_some() {
        ui.spinner().on_hover_text("Finding tracks…");
        if ui.button("Cancel").on_hover_text("Stop finding tracks").clicked() {
            app.workers.cancel_split();
            app.mix.split = None;
        }
        return;
    }
    let Some(listed) = untimed_count(app) else { return };
    let hover = if listed > 0 {
        format!("Estimate from the audio where each of the {listed} listed tracks starts (offline, takes a few seconds). Correct the times where needed.")
    } else {
        "Estimate from the audio where the tracks change (offline, takes a few seconds). The number of tracks is a guess; with a track list in the file's comment tag the times come out much better.".to_owned()
    };
    let can = !app.mix.loading && app.mix.path.is_some();
    if ui.add_enabled(can, egui::Button::new("Find tracks")).on_hover_text(hover).clicked() {
        find_tracks(app, listed);
    }
}

fn find_tracks(app: &mut App, listed: usize) {
    let Some(path) = app.mix.path.clone() else { return };
    let request = app.workers.request_id();
    app.workers.split(request, path, (listed > 0).then_some(listed));
    app.mix.split = Some(Splitting { request, listed });
}

/// The listed entries get the starts in order, as approximate times; without a list each start
/// becomes an unnamed entry. Nothing changes if the list changed while the mix was analysed.
pub fn on_split(app: &mut App, request: u64, result: Result<Vec<f64>, String>) {
    let Some(listed) = app.mix.split.as_ref().filter(|s| s.request == request).map(|s| s.listed) else { return };
    app.mix.split = None;
    let starts = match result {
        Ok(s) => s,
        Err(e) => return app.warn(e),
    };
    if untimed_count(app) != Some(listed) {
        return app.info("The setlist changed while the tracks were being found; nothing was changed");
    }
    let s = app.mix.setlist.get_or_insert_with(Setlist::default);
    if listed > 0 {
        for (e, &t) in s.entries.iter_mut().zip(&starts) {
            (e.start, e.timing) = (Some(t), Timing::Approximate);
        }
    } else {
        s.entries.extend(starts.iter().map(|&t| SetlistEntry::new("", "", Some(t), Provenance::Manual)));
    }
    app.mix.scroll_to_active = true;
    changed(app);
    app.info(match (listed, starts.len()) {
        (0, n) => format!("Found {n} tracks; their number is a guess, so add or delete entries where it is off. The times are estimates (≈)"),
        (l, n) if n < l => format!("Placed {n} of the {l} listed tracks, the mix is too short for more. The times are estimates (≈)"),
        (l, _) => format!("Placed the {l} listed tracks; the times are estimates (≈), correct them where needed"),
    });
}

fn empty_hint(app: &mut App, ui: &mut Ui) {
    ui.add_space(6.0);
    ui.label(RichText::new("No setlist for this file.").color(theme::text_dim()));
    ui.label(
        RichText::new("Nullamp looked for embedded chapters, a cue sheet in the tags, and a .cue file next to it. Import a cue sheet, paste a tracklist copied from a web page, add entries at the playhead, or let Find tracks estimate where the tracks change.")
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

/// A plain click only highlights the row; the editor follows along only when it is already open.
fn click_entry(app: &mut App, id: EntryId) {
    if app.mix.edit.id.is_some() {
        select(app, Some(id));
    } else {
        app.mix.selected = Some(id);
    }
}

/// Select the entry and open it in the editor.
pub(super) fn select(app: &mut App, id: Option<EntryId>) {
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
    // With the editor open the list gives way so that the editor always fits.
    let max_h = if app.mix.edit.id.is_some() { (ui.available_height() * 0.38).max(110.0) } else { (ui.available_height() - 30.0).max(160.0) };
    let scroll_to = std::mem::take(&mut app.mix.scroll_to_active);
    let mut clicked = None;
    let mut seek = None;
    let mut menu_action: Option<(EntryId, &'static str)> = None;
    // The entry Shazam is being asked about, with clips done and total.
    let asking = app.mix.identify.as_ref().map(|i| (i.entry, i.done, i.total));
    ui.spacing_mut().scroll.floating_allocated_width = theme::SCROLL_GUTTER;
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
            let asked = asking.filter(|(id, ..)| *id == e.id);
            // Room on the right for the mark, or for the spinner and clip count.
            let width = rect.right() - text_left - if asked.is_some() { 64.0 } else { 24.0 };
            let title_color = if active { theme::now() } else if e.is_unidentified() { theme::text_faint() } else { theme::text() };
            clip_text(ui, egui::pos2(text_left, rect.top() + 9.0), width, &e.full_title(), theme::prop(12.5), title_color);
            let mut sub = e.display_artist().to_owned();
            if let Some(l) = &e.label {
                sub.push_str(&format!("  [{l}]"));
            }
            clip_text(ui, egui::pos2(text_left, rect.top() + 24.0), width, &sub, theme::prop(11.0), theme::text_dim());
            let mark = if app.mix.candidates.contains_key(&e.id) {
                Some(("?", theme::warn(), "Shazam is unsure: edit the entry to pick one of its answers"))
            } else if e.user_edited {
                Some(("✎", theme::text_dim(), "You corrected this entry"))
            } else if e.verification == Verification::Confirmed {
                Some(("✓", theme::ok(), "Confirmed"))
            } else if e.is_unidentified() {
                Some(("?", theme::text_faint(), "Unidentified track"))
            } else {
                None
            };
            if let Some((_, done, total)) = asked {
                let spot = egui::Rect::from_center_size(egui::pos2(rect.right() - 16.0, rect.center().y), vec2(14.0, 14.0));
                egui::Spinner::new().size(14.0).color(theme::now()).paint_at(ui, spot);
                p.text(egui::pos2(spot.left() - 6.0, rect.center().y), Align2::RIGHT_CENTER, format!("{done}/{total}"), theme::mono(10.0), theme::text_dim());
            } else if let Some((m, c, _)) = mark {
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
                match asked {
                    Some((_, done, total)) => format!("\nAsking Shazam: {done} of {total} clips"),
                    None => mark.map(|(_, _, t)| format!("\n{t}")).unwrap_or_default(),
                }
            );
            let resp = resp.on_hover_text(hover);
            if resp.clicked() {
                clicked = Some(e.id);
            }
            if resp.double_clicked() {
                seek = e.start;
            }
            resp.context_menu(|ui| {
                if ui.button("Edit entry").clicked() {
                    menu_action = Some((e.id, "edit"));
                    ui.close();
                }
                if ui.add_enabled(e.start.is_some(), egui::Button::new("Jump here")).clicked() {
                    seek = e.start;
                    ui.close();
                }
                if ui.button("Set start to playhead").clicked() {
                    menu_action = Some((e.id, "set-start"));
                    ui.close();
                }
                if asked.is_some() {
                    if ui.button("Cancel Shazam").clicked() {
                        menu_action = Some((e.id, "cancel-identify"));
                        ui.close();
                    }
                } else {
                    let why_not = if e.start.is_none() {
                        Some("The entry needs a start time")
                    } else if asking.is_some() {
                        Some("Shazam is being asked about another track; wait for it, or cancel it from that track's menu")
                    } else {
                        None
                    };
                    let hover = why_not.unwrap_or(if fillable(e) {
                        "Name this track with Shazam, about half a minute. Short signatures of a few clips (not the audio) go to Shazam."
                    } else {
                        "Ask Shazam about this track, about half a minute. You named it, so its answer is offered in the editor instead of replacing your name."
                    });
                    if ui.add_enabled(why_not.is_none(), egui::Button::new("Identify with Shazam")).on_hover_text(hover).on_disabled_hover_text(hover).clicked() {
                        menu_action = Some((e.id, "identify"));
                        ui.close();
                    }
                }
                if ui.button("Delete entry").clicked() {
                    menu_action = Some((e.id, "delete"));
                    ui.close();
                }
            });
        }
    });
    if let Some(id) = clicked {
        click_entry(app, id);
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
        Some((id, "edit")) => select(app, Some(id)),
        Some((id, "identify")) => identify_one(app, id),
        Some((_, "cancel-identify")) => cancel_identify(app),
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
        ui.label(RichText::new("Right-click an entry and choose Edit entry to change it. Double-click to jump.").small().color(theme::text_faint()));
        return;
    };
    let pos = app.player.position();
    let mut apply = false;
    let mut cancel = false;
    let mut delete = false;
    let mut moved: Option<i32> = None;
    ui.spacing_mut().scroll.floating_allocated_width = theme::SCROLL_GUTTER;
    egui::ScrollArea::vertical().id_salt("entry-editor").auto_shrink([false, true]).show(ui, |ui| {
    egui::Frame::new().fill(theme::raised()).corner_radius(4.0).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
        // Save and Cancel stay at the top, in view however small the panel.
        ui.horizontal(|ui| {
            if ui.button("Save").clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)) && ui.ctx().egui_wants_keyboard_input() {
                apply = true;
            }
            if ui.button("Cancel").on_hover_text("Close without saving").clicked() {
                cancel = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(RichText::new("Delete").color(theme::err())).clicked() {
                    delete = true;
                }
            });
        });
        let ed = &mut app.mix.edit;
        // Above the fields, so the choice is in view however small the panel.
        if let Some(songs) = app.mix.candidates.get(&id) {
            ui.label(RichText::new("Shazam is unsure; it named").small().color(theme::warn()));
            for song in songs {
                ui.horizontal(|ui| {
                    if ui.small_button("Use").on_hover_text("Put this artist and title in the fields; Save keeps them").clicked() {
                        ed.artist = song.artist.clone();
                        ed.title = song.title.clone();
                        ed.version.clear();
                    }
                    if let Some(url) = &song.url {
                        ui.hyperlink_to(RichText::new("open").small(), url).on_hover_text("The song's page on shazam.com");
                    }
                    ui.add(egui::Label::new(RichText::new(format!("{} - {}", song.artist, song.title)).small()).truncate());
                });
            }
            ui.add_space(4.0);
        }
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
            if ui.small_button("▲").clicked() {
                moved = Some(-1);
            }
            if ui.small_button("▼").clicked() {
                moved = Some(1);
            }
        });
    });
    });
    if apply && apply_edit(app, id) || cancel {
        select(app, None);
        return;
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

/// Write the editor's fields into the entry; false (with the error shown in the editor) when a time does not parse.
fn apply_edit(app: &mut App, id: EntryId) -> bool {
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
            return false;
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
    app.mix.candidates.remove(&id);
    changed(app);
    true
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
        let (incoming, ignored) = if cue::looks_like_cue(&paste.text) {
            let audio_name = app.mix.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            (cue_setlist(&paste.text, &audio_name, "pasted cue sheet").unwrap_or_default(), 0)
        } else {
            let parsed = tracklist_text::parse(&paste.text, url);
            (parsed.setlist, parsed.ignored.len())
        };
        let timed = incoming.entries.iter().filter(|e| e.start.is_some()).count();
        ui.label(
            RichText::new(format!("{} tracks recognised ({timed} with times), {ignored} lines ignored", incoming.entries.len()))
                .small()
                .color(if incoming.entries.is_empty() { theme::warn() } else { theme::ok() }),
        );
        if app.mix.setlist.as_ref().is_some_and(|s| !s.entries.is_empty()) {
            ui.checkbox(&mut paste.replace, "Replace current entries (your corrections are kept)");
        }
        ui.horizontal(|ui| {
            if ui.add_enabled(!incoming.entries.is_empty(), egui::Button::new("Import")).clicked() {
                apply_incoming(app, incoming.clone(), paste.replace);
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
    if app.mix.confirm_cue_comment {
        let entries = app.mix.setlist.as_ref().map_or(&[][..], |s| &s.entries[..]);
        let unconfirmed = entries.iter().filter(|e| e.verification != Verification::Confirmed).count();
        let estimated = entries.iter().filter(|e| e.timing == Timing::Approximate).count();
        let tracks = entries.len();
        egui::Modal::new(egui::Id::unique("write-cue-comment")).show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.heading("Write the setlist into the comment?");
            ui.label(format!(
                "The file's comment is replaced by a cue sheet of these {tracks} tracks, with their start times, artists and titles. Nullamp reads it back when the file is played anywhere. Other tags and the audio are preserved. A backup copy is kept if backups are enabled."
            ));
            if unconfirmed > 0 || estimated > 0 {
                ui.add_space(4.0);
            }
            if unconfirmed > 0 {
                ui.label(RichText::new(format!("{unconfirmed} of {tracks} entries are not confirmed yet.")).color(theme::warn()));
            }
            if estimated > 0 {
                ui.label(RichText::new(format!("{estimated} of {tracks} start times are estimates (≈).")).color(theme::warn()));
            }
            ui.horizontal(|ui| {
                if ui.button("Write cue sheet").clicked() {
                    app.mix.confirm_cue_comment = false;
                    super::inspector::write_cue_comment(app);
                }
                if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    app.mix.confirm_cue_comment = false;
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;

    use super::*;
    use crate::ui_snapshots::{seeded_store, settle};

    fn song(artist: &str, title: &str) -> Song {
        Song { artist: artist.into(), title: title.into(), key: title.into(), url: Some(format!("https://www.shazam.com/track/{title}")), offset: 0.0, time_skew: 0.0, frequency_skew: 0.0 }
    }

    /// Answers fill entries nobody named, as suggestions; a name the user typed stays, and
    /// unsure answers wait in the editor. Nothing here asks Shazam.
    #[test]
    fn identify_answers_fill_unnamed_entries_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = seeded_store(dir.path(), "dark");
        let mut h = Harness::builder().with_size(egui::vec2(1180.0, 740.0)).build_eframe(|cc| App::new(cc, store));
        {
            let app = h.state_mut();
            let id = app.player.lists[0].playlist.entries()[5].id;
            app.player.play_entry(0, id, 0.0);
        }
        settle(&mut h, 1500);
        let app = h.state_mut();
        let ids: Vec<EntryId> = app.mix.setlist.as_ref().expect("the mix fixture has chapters").entries.iter().map(|e| e.id).collect();
        assert!(ids.len() >= 3, "{} chapters", ids.len());
        // The second entry was named by the user, the third is a marker from "+ At playhead".
        {
            let entries = &mut app.mix.setlist.as_mut().unwrap().entries;
            (entries[1].artist, entries[1].title, entries[1].user_edited) = ("Mine".into(), "Typed".into(), true);
            (entries[2].artist, entries[2].title, entries[2].user_edited) = (String::new(), String::new(), true);
        }
        // One entry is asked about, up to where the next one starts.
        let starts: Vec<Option<f64>> = app.mix.setlist.as_ref().unwrap().entries.iter().map(|e| e.start).collect();
        let span = identify_span(app, ids[1]).unwrap();
        assert_eq!((Some(span.start), Some(span.end)), (starts[1], starts[2]));

        // Each answer belongs to its own request, as the right-click item starts one per entry.
        let ask = |app: &mut App, request: u64, entry: EntryId, verdict: Verdict| -> Option<String> {
            app.mix.identify = Some(Identifying { request, entry, done: 5, total: 5, outcome: None });
            on_identified(app, request, entry, verdict);
            on_identify_done(app, request, Ok(()));
            assert!(app.mix.identify.is_none());
            app.message.as_ref().map(|m| m.text.clone())
        };
        let said = ask(app, 7, ids[0], Verdict::Named { song: song("Donnie", "Troopa"), support: 3 });
        assert_eq!(said.as_deref(), Some("Shazam named the track Donnie - Troopa"));
        let said = ask(app, 8, ids[1], Verdict::Named { song: song("Other", "Song"), support: 2 });
        assert!(said.is_some_and(|m| m.contains("edit the entry")));
        ask(app, 9, ids[2], Verdict::Unsure { candidates: vec![song("A", "One"), song("B", "Two")] });
        // An answer to a request that is no longer running is dropped.
        app.mix.identify = Some(Identifying { request: 11, entry: ids[0], done: 0, total: 5, outcome: None });
        on_identified(app, 10, ids[0], Verdict::Named { song: song("Stale", "Answer"), support: 5 });

        let entries = &app.mix.setlist.as_ref().unwrap().entries;
        assert_eq!((entries[0].artist.as_str(), entries[0].title.as_str()), ("Donnie", "Troopa"));
        assert_eq!(entries[0].verification, Verification::Suggested);
        assert_eq!(entries[0].provenance, Provenance::Online { provider: "Shazam".into(), url: Some("https://www.shazam.com/track/Troopa".into()) });
        assert_eq!(entries[1].title, "Typed", "a name the user typed stays");
        assert_eq!(app.mix.candidates.get(&ids[1]).map(|c| c[0].title.as_str()), Some("Song"), "and the answer is offered instead");
        assert!(entries[2].title.is_empty(), "an unsure answer changes nothing by itself");
        assert_eq!(app.mix.candidates.get(&ids[2]).map(Vec::len), Some(2));
        assert_eq!(app.mix.origin, Some(SetlistOrigin::Saved), "the filled setlist is saved");
        h.state_mut().player.stop();
    }

    fn tag_list(names: &[&str]) -> Setlist {
        Setlist { entries: names.iter().map(|n| SetlistEntry::new("Artist", *n, None, Provenance::TagDescription)).collect(), ..Default::default() }
    }

    /// Found starts go to the listed tracks in order, or become unnamed entries without a list;
    /// an answer for a list that has changed meanwhile, or for another request, changes nothing.
    #[test]
    fn found_starts_time_the_list_or_make_entries() {
        let dir = tempfile::tempdir().unwrap();
        let store = seeded_store(dir.path(), "dark");
        let mut h = Harness::builder().with_size(egui::vec2(1180.0, 740.0)).build_eframe(|cc| App::new(cc, store));
        {
            let app = h.state_mut();
            let id = app.player.lists[0].playlist.entries()[5].id;
            app.player.play_entry(0, id, 0.0);
        }
        settle(&mut h, 1500);
        let app = h.state_mut();
        assert_eq!(untimed_count(app), None, "the mix fixture has chapters, which are not touched");

        app.mix.setlist = Some(tag_list(&["One", "Two", "Three"]));
        assert_eq!(untimed_count(app), Some(3));
        app.mix.split = Some(Splitting { request: 5, listed: 3 });
        on_split(app, 4, Ok(vec![0.0, 1.0, 2.0]));
        assert!(app.mix.split.is_some(), "another request's answer is dropped");
        on_split(app, 5, Ok(vec![0.0, 64.0, 130.0]));
        let entries = &app.mix.setlist.as_ref().unwrap().entries;
        assert_eq!(entries.iter().map(|e| e.start).collect::<Vec<_>>(), [Some(0.0), Some(64.0), Some(130.0)]);
        assert!(entries.iter().all(|e| e.timing == Timing::Approximate && !e.user_edited));
        assert_eq!(entries[1].title, "Two");
        assert_eq!(app.mix.origin, Some(SetlistOrigin::Saved));
        assert_eq!(untimed_count(app), None, "a timed list is not offered again");

        app.mix.setlist = None;
        app.mix.split = Some(Splitting { request: 6, listed: 0 });
        on_split(app, 6, Ok(vec![0.0, 300.0]));
        let entries = &app.mix.setlist.as_ref().unwrap().entries;
        assert_eq!(entries.iter().map(|e| e.start).collect::<Vec<_>>(), [Some(0.0), Some(300.0)]);
        assert!(entries.iter().all(|e| e.is_unidentified()));

        // The user pasted a list while the mix was analysed without one.
        app.mix.setlist = Some(tag_list(&["One", "Two"]));
        app.mix.split = Some(Splitting { request: 7, listed: 0 });
        on_split(app, 7, Ok(vec![0.0, 300.0]));
        assert!(app.mix.setlist.as_ref().unwrap().entries.iter().all(|e| e.start.is_none()));
        assert!(app.mix.split.is_none());
        h.state_mut().player.stop();
    }

    #[test]
    fn found_times_survive_a_rewrite_of_the_tag() {
        let mut current = tag_list(&["One", "Two", "Three"]).entries;
        for (e, t) in current.iter_mut().zip([0.0, 64.0, 130.0]) {
            e.start = Some(t);
        }
        // A name fixed in place keeps its time.
        let mut incoming = tag_list(&["One", "Tow", "Three"]).entries;
        keep_found_times(&current, &mut incoming);
        assert_eq!(incoming.iter().map(|e| e.start).collect::<Vec<_>>(), [Some(0.0), Some(64.0), Some(130.0)]);
        // With a track added, times follow the names.
        let mut incoming = tag_list(&["One", "New", "Two", "Three"]).entries;
        keep_found_times(&current, &mut incoming);
        assert_eq!(incoming.iter().map(|e| e.start).collect::<Vec<_>>(), [Some(0.0), None, Some(64.0), Some(130.0)]);
    }
}

/// A setlist entry while Shazam is being asked about it, and the entry editor offering candidates.
#[cfg(test)]
#[test]
#[ignore]
fn render_identify() {
    use egui_kittest::Harness;

    use crate::ui_snapshots::{seeded_store, settle};

    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(1180.0, 740.0)).with_pixels_per_point(2.0).with_theme(egui::Theme::Dark).wgpu().build_eframe(|cc| App::new(cc, store));
    {
        let app = h.state_mut();
        let id = app.player.lists[0].playlist.entries()[5].id;
        app.player.play_entry(0, id, 0.0);
    }
    settle(&mut h, 1500);
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/screenshots");
    std::fs::create_dir_all(&out).unwrap();
    let save = |h: &mut Harness<'_, App>, name: &str| h.render().expect("render").save(out.join(name)).unwrap();
    let ids: Vec<EntryId> = h.state().mix.setlist.as_ref().unwrap().entries.iter().map(|e| e.id).collect();
    let song = |artist: &str, title: &str| Song { artist: artist.into(), title: title.into(), key: title.into(), url: Some("https://www.shazam.com/".into()), offset: 0.0, time_skew: 0.0, frequency_skew: 0.0 };
    {
        let app = h.state_mut();
        app.mix.identify = Some(Identifying { request: 7, entry: ids[0], done: 0, total: 5, outcome: None });
        on_identified(app, 7, ids[0], Verdict::Named { song: song("Donnie", "Troopa (feat. Yung Felix)"), support: 3 });
        on_identify_done(app, 7, Ok(()));
        app.mix.identify = Some(Identifying { request: 8, entry: ids[1], done: 2, total: 5, outcome: None });
    }
    settle(&mut h, 300);
    save(&mut h, "14b-identify-running.png");
    {
        let app = h.state_mut();
        on_identified(app, 8, ids[1], Verdict::Unsure { candidates: vec![song("Trilucid & Phil Martyn", "Gone"), song("Bingo Players", "Devotion (feat. Tony Scott)")] });
        on_identify_done(app, 8, Ok(()));
    }
    crate::ui::select_setlist_entry(h.state_mut(), 1);
    settle(&mut h, 300);
    save(&mut h, "14c-identify-candidates.png");
    h.state_mut().player.stop();
}

/// "Find tracks" offered for an untimed track list, running, and the list with the found times.
#[cfg(test)]
#[test]
#[ignore]
fn render_find_tracks() {
    use egui_kittest::Harness;

    use crate::ui_snapshots::{seeded_store, settle};

    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(1180.0, 740.0)).with_pixels_per_point(2.0).with_theme(egui::Theme::Dark).wgpu().build_eframe(|cc| App::new(cc, store));
    {
        let app = h.state_mut();
        let id = app.player.lists[0].playlist.entries()[5].id;
        app.player.play_entry(0, id, 0.0);
    }
    settle(&mut h, 1500);
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/screenshots");
    std::fs::create_dir_all(&out).unwrap();
    let save = |h: &mut Harness<'_, App>, name: &str| h.render().expect("render").save(out.join(name)).unwrap();
    let names = [("Pablo Bolivar", "Senda (Luca Bacchetti Endless Remix)"), ("Dhany G", "Sun 24"), ("Jeremy Olander", "Osho"), ("Valleraphon", "Shade (Paul Sawyer Remix)")];
    h.state_mut().mix.setlist = Some(Setlist { entries: names.iter().map(|(a, t)| SetlistEntry::new(*a, *t, None, Provenance::TagDescription)).collect(), ..Default::default() });
    settle(&mut h, 300);
    save(&mut h, "15a-find-tracks-offered.png");
    h.state_mut().mix.split = Some(Splitting { request: 9, listed: 4 });
    settle(&mut h, 300);
    save(&mut h, "15b-find-tracks-running.png");
    on_split(h.state_mut(), 9, Ok(vec![0.0, 318.0, 432.0, 952.0]));
    settle(&mut h, 300);
    save(&mut h, "15c-find-tracks-done.png");
    h.state_mut().mix.confirm_cue_comment = true;
    settle(&mut h, 300);
    save(&mut h, "15d-write-cue-comment.png");
    h.state_mut().player.stop();
}
