//! "Import from Spotify": a public playlist becomes a playlist of tracks that have no file yet.
//!
//! Only the names are taken (artist, title, length), read from Spotify's public embed page by
//! `nullamp-spotify` without an account. Every track starts out missing; the user finds the files.
//! Each one gets the path the file would have if saved in the Music folder as
//! `Spotify/<playlist>/<artist> - <title>.mp3`, so a file saved there under that name plays after a
//! rescan, and the row says which song is meant until then.

use std::path::{Path, PathBuf};

use egui::RichText;
use nullamp_core::{PlaylistEntry, TrackInfo};
use nullamp_spotify::{PAGE, Playlist, Track};

use super::{App, dialogs::sanitize, playlist_view, theme, widgets};

/// The dialog, from the link being typed to the answer coming back.
#[derive(Default)]
pub struct Import {
    pub link: String,
    /// The request being waited for.
    pub request: Option<u64>,
    pub error: Option<String>,
}

pub fn open(app: &mut App) {
    app.dialogs.spotify = Some(Import::default());
}

pub fn modal(app: &mut App, ctx: &egui::Context, enter: bool, escape: bool) {
    let Some(mut import) = app.dialogs.spotify.take() else { return };
    let (mut keep, mut run) = (true, false);
    let busy = import.request.is_some();
    egui::Modal::new(egui::Id::unique("import-spotify")).show(ctx, |ui| {
        ui.set_width(widgets::modal_width(ctx, 480.0));
        ui.heading("Import from Spotify");
        ui.label(RichText::new("Paste the link of a public playlist (in Spotify: Share, Copy link to playlist). No account is needed.").small().color(theme::text_dim()));
        let field = ui.add_enabled(!busy, egui::TextEdit::singleline(&mut import.link).hint_text("https://open.spotify.com/playlist/…").desired_width(f32::INFINITY));
        if !busy {
            field.request_focus();
        }
        if let Some(error) = &import.error {
            ui.label(RichText::new(error).small().color(theme::err()));
        }
        if busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Reading the playlist…").small().color(theme::text_dim()));
            });
        }
        ui.horizontal(|ui| {
            let ready = !busy && !import.link.trim().is_empty();
            if ui.add_enabled(ready, egui::Button::new("Import")).clicked() || (ready && enter) {
                run = true;
            }
            if ui.button("Cancel").clicked() || escape {
                keep = false;
            }
        });
        ui.label(RichText::new(format!("Only the tracks' names are brought in, and every track starts out missing for you to find. Spotify shows the first {PAGE} tracks of a playlist and no more.")).small().color(theme::text_dim()));
    });
    if run {
        let request = app.workers.request_id();
        import.request = Some(request);
        import.error = None;
        app.workers.fetch_spotify(request, import.link.clone());
    }
    if keep {
        app.dialogs.spotify = Some(import);
    }
}

/// The answer to a request. One that is no longer waited for (the dialog was closed) is dropped.
pub fn on_playlist(app: &mut App, request: u64, result: Result<Playlist, String>) {
    let Some(import) = app.dialogs.spotify.as_mut().filter(|i| i.request == Some(request)) else { return };
    import.request = None;
    match result {
        Ok(list) if list.tracks.is_empty() => import.error = Some("The playlist has no tracks".into()),
        Ok(list) => {
            app.dialogs.spotify = None;
            add_playlist(app, list);
        }
        Err(e) => import.error = Some(e),
    }
}

fn add_playlist(app: &mut App, list: Playlist) {
    let folder = app.store.paths.music().join("Spotify").join(sanitize(&list.name));
    let entries: Vec<PlaylistEntry> = list.tracks.iter().map(|t| PlaylistEntry::with_info(track_path(&folder, t), track_info(t))).collect();
    let paths: Vec<PathBuf> = entries.iter().map(|e| e.path.clone()).collect();
    playlist_view::new_playlist(app, &list.name);
    let at = app.player.viewing;
    app.insert_entries(at, entries, None);
    // Marks them missing, as for an imported M3U.
    app.workers.check_exists(paths);
    let n = list.tracks.len();
    if list.cut {
        app.warn(format!("Imported the first {n} tracks of \"{}\"; Spotify shows no more of a playlist. None has a file yet", list.name));
    } else {
        app.info(format!("Imported {n} track{} of \"{}\"; none has a file yet", if n == 1 { "" } else { "s" }, list.name));
    }
}

fn track_info(track: &Track) -> TrackInfo {
    TrackInfo { title: Some(track.title.clone()), artist: Some(track.artist.clone()).filter(|a| !a.is_empty()), duration: track.duration, ..Default::default() }
}

/// Where the file of `track` would be in `folder`. The name is cut well short of what a file system allows.
fn track_path(folder: &Path, track: &Track) -> PathBuf {
    let name = if track.artist.is_empty() { track.title.clone() } else { format!("{} - {}", track.artist, track.title) };
    let name: String = sanitize(&name).chars().take(150).collect();
    folder.join(format!("{}.mp3", name.trim()))
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use nullamp_core::EntryStatus;

    use super::*;
    use crate::ui::MessageKind;
    use crate::ui_snapshots::{seeded_store, settle};

    fn track(artist: &str, title: &str) -> Track {
        Track { artist: artist.into(), title: title.into(), duration: Some(215.0), uri: format!("spotify:track:{title}") }
    }

    fn playlist(name: &str, tracks: Vec<Track>, cut: bool) -> Playlist {
        Playlist { name: name.into(), id: "37i9dQZF1DXcBWIGoYBM5M".into(), tracks, cut }
    }

    #[test]
    fn a_track_has_the_path_its_file_would_have() {
        let folder = Path::new("/Music/Spotify/Late night");
        assert_eq!(track_path(folder, &track("Queen, David Bowie", "Under Pressure")), folder.join("Queen, David Bowie - Under Pressure.mp3"));
        assert_eq!(track_path(folder, &track("AC/DC", "Who Made Who?")), folder.join("AC_DC - Who Made Who_.mp3"));
        assert_eq!(track_path(folder, &track("", "Interlude")), folder.join("Interlude.mp3"));
        let long = track_path(folder, &track("A", &"x".repeat(400)));
        assert!(long.file_name().unwrap().len() < 200);
    }

    #[test]
    fn a_track_keeps_what_spotify_says_of_it() {
        let info = track_info(&track("Tate McRae", "Housewife"));
        assert_eq!((info.title.as_deref(), info.artist.as_deref(), info.duration), (Some("Housewife"), Some("Tate McRae"), Some(215.0)));
        assert_eq!(track_info(&track("", "Interlude")).artist, None);
    }

    /// The playlist is made and shown, its tracks are named and missing; a cut one is warned about;
    /// failures stay in the dialog, and an answer nobody waits for any more changes nothing.
    #[test]
    fn the_answer_becomes_a_playlist_of_missing_tracks() {
        let dir = tempfile::tempdir().unwrap();
        let store = seeded_store(dir.path(), "dark");
        let mut h = Harness::builder().with_size(egui::vec2(1180.0, 740.0)).build_eframe(|cc| App::new(cc, store));
        settle(&mut h, 300);
        let app = h.state_mut();
        let lists = app.player.lists.len();

        open(app);
        app.dialogs.spotify.as_mut().unwrap().request = Some(7);
        on_playlist(app, 7, Err("Spotify has no public playlist with this link; it may be private".into()));
        let shown = app.dialogs.spotify.as_ref().unwrap();
        assert!(shown.request.is_none() && shown.error.as_deref().is_some_and(|e| e.contains("private")));

        app.dialogs.spotify.as_mut().unwrap().request = Some(8);
        on_playlist(app, 8, Ok(playlist("Empty", Vec::new(), false)));
        assert_eq!(app.dialogs.spotify.as_ref().unwrap().error.as_deref(), Some("The playlist has no tracks"));
        assert_eq!(app.player.lists.len(), lists);

        app.dialogs.spotify.as_mut().unwrap().request = Some(9);
        on_playlist(app, 4, Ok(playlist("Stale", vec![track("A", "One")], false)));
        assert_eq!(app.player.lists.len(), lists, "an answer to another request is dropped");

        on_playlist(app, 9, Ok(playlist("Late night", vec![track("Queen, David Bowie", "Under Pressure"), track("Tate McRae", "Housewife")], false)));
        assert!(app.dialogs.spotify.is_none());
        assert_eq!(app.player.lists.len(), lists + 1);
        let list = &app.player.lists[app.player.viewing].playlist;
        assert_eq!(list.name, "Late night");
        let entries = list.entries();
        assert_eq!(entries.len(), 2);
        assert_eq!((entries[0].info.artist.as_deref(), entries[0].info.title.as_deref(), entries[0].info.duration), (Some("Queen, David Bowie"), Some("Under Pressure"), Some(215.0)));
        assert!(entries[0].path.ends_with("Spotify/Late night/Queen, David Bowie - Under Pressure.mp3"));
        assert!(entries.iter().all(|e| !e.path.exists()));
        assert!(matches!(h.state().message.as_ref().map(|m| m.kind), Some(MessageKind::Info)));

        // The existence check runs on a worker.
        settle(&mut h, 300);
        let list = &h.state().player.lists[h.state().player.viewing].playlist;
        assert!(list.entries().iter().all(|e| e.status == EntryStatus::Missing));

        let app = h.state_mut();
        open(app);
        app.dialogs.spotify.as_mut().unwrap().request = Some(10);
        let tracks: Vec<Track> = (0..PAGE).map(|i| track("A", &format!("T{i}"))).collect();
        on_playlist(app, 10, Ok(playlist("Long", tracks, true)));
        assert_eq!(app.player.viewed().len(), PAGE);
        let message = app.message.as_ref().unwrap();
        assert!(matches!(message.kind, MessageKind::Warn) && message.text.contains("first 100"), "{}", message.text);
    }
}
