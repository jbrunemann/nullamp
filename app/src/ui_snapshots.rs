//! Offscreen renders of the real UI for visual review.
//! Requires a GPU (wgpu): cargo test -p nullamp ui_snapshots -- --ignored
//! Images are written to target/screenshots/. Add `--test-threads=1`: the theme
//! palette is process-global, so the dark and light renders must not overlap.

use std::path::{Path, PathBuf};
use std::time::Duration;

use egui_kittest::Harness;

use crate::paths::AppPaths;
use crate::store::{ArtworkMode, PlaylistDoc, Store};
use crate::ui::App;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures")
}

fn out_dir() -> PathBuf {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/screenshots");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn seeded_store(dir: &Path, appearance: &str) -> Store {
    let paths = AppPaths { config: dir.join("config"), data: dir.join("data"), cache: dir.join("cache") };
    let store = Store::new(paths);
    let f = fixtures();
    let mut pl = nullamp_core::Playlist::new("Default");
    pl.add_paths(["tagged_v23.mp3", "tagged_v24.mp3", "tone.flac", "tone.ogg", "tone.m4a", "mix_chapters.mp3", "missing.mp3", "tone_mono_22k.wav"].iter().map(|n| f.join(n)));
    store.save_playlist(&PlaylistDoc { key: "default".into(), playlist: pl }).unwrap();
    store.save_playlist(&PlaylistDoc { key: "dj-mixes".into(), playlist: nullamp_core::Playlist::new("DJ Mixes") }).unwrap();
    let settings = crate::store::Settings { volume: 0.0, appearance: appearance.into(), ..Default::default() };
    store.save_settings(&settings).unwrap();
    store
}

fn settle(h: &mut Harness<'_, App>, ms: u64) {
    for _ in 0..(ms / 50) {
        std::thread::sleep(Duration::from_millis(50));
        h.step();
    }
}

fn save(h: &mut Harness<'_, App>, name: &str) {
    let img = h.render().expect("render");
    img.save(out_dir().join(name)).unwrap();
}

#[test]
#[ignore]
fn render_main_window() {
    render_main_window_as("dark", "");
}

#[test]
#[ignore]
fn render_main_window_light() {
    render_main_window_as("light", "-light");
}

fn render_main_window_as(appearance: &str, suffix: &str) {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), appearance);
    // The harness forces its own theme after the app's setup, so pass it here too.
    let theme = if appearance == "light" { egui::Theme::Light } else { egui::Theme::Dark };
    let mut h = Harness::builder().with_size(egui::vec2(1180.0, 740.0)).with_pixels_per_point(2.0).with_theme(theme).wgpu().build_eframe(|cc| App::new(cc, store));
    settle(&mut h, 800);
    save(&mut h, &format!("01-idle{suffix}.png"));

    // Play the mix fixture (volume is zero) so the setlist and deck fill in.
    {
        let app = h.state_mut();
        let id = app.player.lists[0].playlist.entries()[5].id;
        app.player.play_entry(0, id, 0.0);
    }
    settle(&mut h, 1500);
    {
        let app = h.state_mut();
        app.player.seek_to(2.6);
        let id = app.player.lists[0].playlist.entries()[5].id;
        app.selection.ids = [id].into();
        app.selection.cursor = Some(id);
    }
    settle(&mut h, 800);
    save(&mut h, &format!("02-mix-setlist{suffix}.png"));

    crate::ui::select_setlist_entry(h.state_mut(), 1);
    settle(&mut h, 300);
    save(&mut h, &format!("02b-entry-editor{suffix}.png"));
    crate::ui::select_setlist_entry(h.state_mut(), usize::MAX);

    {
        let app = h.state_mut();
        app.side_tab = crate::ui::SideTab::Inspector;
        crate::ui::inspect_selection(app);
    }
    settle(&mut h, 1200);
    save(&mut h, &format!("03-inspector{suffix}.png"));

    // A file with embedded artwork, to look at the artwork section.
    {
        let app = h.state_mut();
        let id = app.player.lists[0].playlist.entries()[0].id;
        app.selection.ids = [id].into();
        app.selection.cursor = Some(id);
        crate::ui::inspect_selection(app);
    }
    settle(&mut h, 1200);
    save(&mut h, &format!("03b-inspector-artwork{suffix}.png"));
    h.state_mut().player.stop();
}

#[test]
#[ignore]
fn render_dim_stages() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(1180.0, 740.0)).with_pixels_per_point(2.0).with_theme(egui::Theme::Dark).wgpu().build_eframe(|cc| App::new(cc, store));
    settle(&mut h, 800);
    {
        let app = h.state_mut();
        let id = app.player.lists[0].playlist.entries()[5].id;
        app.player.play_entry(0, id, 0.0);
    }
    settle(&mut h, 1500);
    for level in 0..=crate::ui::theme::DIM_LEVELS {
        h.state_mut().settings.dim = level;
        settle(&mut h, 200);
        save(&mut h, &format!("04-dim-{level}.png"));
    }
    h.state_mut().player.stop();
}

#[test]
#[ignore]
fn render_playlist_columns() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(1180.0, 420.0)).with_pixels_per_point(2.0).with_theme(egui::Theme::Dark).wgpu().build_eframe(|cc| App::new(cc, store));
    settle(&mut h, 1200);
    save(&mut h, "04-columns-default.png");
    // Artist before title, album hidden.
    h.state_mut().settings.playlist_columns = ["num", "art", "artist", "title", "time"].map(String::from).to_vec();
    settle(&mut h, 200);
    save(&mut h, "04b-columns-reordered.png");
    h.state_mut().settings.playlist_columns = ["title", "artist", "album", "year", "time"].map(String::from).to_vec();
    settle(&mut h, 200);
    save(&mut h, "04c-columns-no-art.png");
}

#[test]
#[ignore]
fn render_player_only() {
    for (width, name) in [(1100.0, "04-player-only.png"), (640.0, "04-player-narrow.png")] {
        render_player_only_at(width, name);
    }
}

fn render_player_only_at(width: f32, name: &str) {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(width, 260.0)).with_pixels_per_point(2.0).wgpu().build_eframe(|cc| App::new(cc, store));
    {
        let app = h.state_mut();
        app.playlist_open = false;
        app.side_open = false;
        app.settings.visualizer = "off".into();
        let id = app.player.lists[0].playlist.entries()[5].id;
        app.player.play_entry(0, id, 0.0);
    }
    settle(&mut h, 1500);
    h.state_mut().player.seek_to(2.6);
    settle(&mut h, 400);
    save(&mut h, name);
    h.state_mut().player.stop();
}

#[test]
fn ui_glyphs_exist_in_bundled_fonts() {
    let ctx = egui::Context::default();
    crate::ui::theme::apply(&ctx, crate::ui::theme::Appearance::Dark);
    let mut out = ctx.run_ui(Default::default(), |_| {});
    out.textures_delta.clear();
    // Every non-ASCII character in the UI sources (comments excluded).
    let mut used = std::collections::BTreeSet::new();
    for f in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/src/ui")).unwrap() {
        let text = std::fs::read_to_string(f.unwrap().path()).unwrap();
        for line in text.lines().filter(|l| !l.trim_start().starts_with("//")) {
            used.extend(line.chars().filter(|c| !c.is_ascii()));
        }
    }
    let missing: Vec<char> = ctx.fonts_mut(|f| {
        used.iter()
            .copied()
            .filter(|c| !f.has_glyphs(&egui::FontId::proportional(12.0), &c.to_string()) || !f.has_glyphs(&egui::FontId::monospace(12.0), &c.to_string()))
            .collect()
    });
    let mut out = ctx.run_ui(Default::default(), |_| {});
    out.textures_delta.clear();
    assert!(missing.is_empty(), "glyphs missing from bundled fonts: {missing:?}");
}

/// Rewriting the Comment tag of the playing file reloads the setlist read from it,
/// keeping the user's corrections.
#[test]
fn tag_edit_reloads_setlist_from_comment() {
    use nullamp_meta::{Edit, Field, WriteOptions};

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("mix.mp3");
    std::fs::copy(fixtures().join("tagged_v24.mp3"), &file).unwrap();
    let write_comment = |text: &str| {
        let edit = Edit { fields: [(Field::Comment, Some(text.to_owned()))].into(), ..Default::default() };
        nullamp_meta::write(&file, nullamp_meta::stamp(&file).unwrap(), &edit, &WriteOptions::default()).unwrap();
    };
    write_comment("1. A - One\n2. B - Two\n3. C - Three\n");

    let paths = AppPaths { config: dir.path().join("config"), data: dir.path().join("data"), cache: dir.path().join("cache") };
    let store = Store::new(paths);
    let mut pl = nullamp_core::Playlist::new("Default");
    pl.add_paths([file.clone()]);
    store.save_playlist(&PlaylistDoc { key: "default".into(), playlist: pl }).unwrap();
    store.save_settings(&crate::store::Settings { volume: 0.0, ..Default::default() }).unwrap();

    let mut h = Harness::builder().with_size(egui::vec2(900.0, 600.0)).build_eframe(|cc| App::new(cc, store));
    {
        let app = h.state_mut();
        let id = app.player.lists[0].playlist.entries()[0].id;
        app.player.play_entry(0, id, 0.0);
    }
    let titles = |h: &Harness<'_, App>| -> Vec<String> {
        h.state().mix.setlist.as_ref().map(|s| s.entries.iter().map(|e| e.title.clone()).collect()).unwrap_or_default()
    };
    let refresh = |h: &mut Harness<'_, App>| {
        let app = h.state_mut();
        let c = app.player.current.clone().unwrap();
        app.workers.submit(crate::workers::Job::NowPlaying { token: c.token, path: c.path });
        settle(h, 600);
    };
    settle(&mut h, 1200);
    assert_eq!(titles(&h), ["One", "Two", "Three"]);

    write_comment("1. A - One\n2. B - Two\n3. C - Three\n4. D - Four\n");
    refresh(&mut h);
    assert_eq!(titles(&h), ["One", "Two", "Three", "Four"]);
    assert_eq!(h.state().mix.origin, Some(crate::workers::SetlistOrigin::Discovered));

    // A corrected entry survives; the others follow the tag.
    h.state_mut().mix.setlist.as_mut().unwrap().entries[1].user_edited = true;
    h.state_mut().mix.origin = Some(crate::workers::SetlistOrigin::Saved);
    write_comment("1. X - Ten\n2. Y - Eleven\n3. Z - Twelve\n");
    refresh(&mut h);
    let t = titles(&h);
    assert!(t.contains(&"Two".to_owned()), "{t:?}");
    assert!(t.contains(&"Eleven".to_owned()) && !t.contains(&"One".to_owned()) && !t.contains(&"Four".to_owned()), "{t:?}");

    write_comment("Just a note.\n");
    refresh(&mut h);
    assert_eq!(titles(&h), ["Two"], "only the correction remains");

    // A cue sheet pasted into the comment loads as a timed setlist.
    write_comment("FILE \"mix.mp3\" MP3\n TRACK 01 AUDIO\n TITLE \"Alpha\"\n PERFORMER \"P\"\n INDEX 01 00:00:00\n TRACK 02 AUDIO\n TITLE \"Beta\"\n PERFORMER \"P\"\n INDEX 01 01:00:00\n");
    refresh(&mut h);
    let t = titles(&h);
    assert!(t.contains(&"Alpha".to_owned()) && t.contains(&"Beta".to_owned()), "{t:?}");
    let beta = h.state().mix.setlist.as_ref().unwrap().entries.iter().find(|e| e.title == "Beta").unwrap().start;
    assert_eq!(beta, Some(60.0));
    h.state_mut().player.stop();
}

/// The waveform of the playing track arrives in the background and is cached; the next
/// launch prunes cache entries that have gone unused for too long.
#[test]
fn waveform_arrives_and_old_cache_entries_are_pruned() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let cache = store.paths.waveform_cache();
    std::fs::create_dir_all(&cache).unwrap();
    let stale = cache.join("0000000000000001.wf");
    std::fs::write(&stale, b"NWF1").unwrap();
    let long_ago = std::time::SystemTime::now() - crate::waveform_cache::MAX_AGE - Duration::from_secs(3600);
    std::fs::File::options().write(true).open(&stale).unwrap().set_modified(long_ago).unwrap();

    let mut h = Harness::builder().with_size(egui::vec2(900.0, 600.0)).build_eframe(|cc| App::new(cc, store));
    let played = {
        let app = h.state_mut();
        let entry = &app.player.lists[0].playlist.entries()[2];
        let (id, path) = (entry.id, entry.path.clone());
        app.player.play_entry(0, id, 0.0);
        path
    };
    assert!(h.state().now.waveform.is_none());
    settle(&mut h, 1000);
    let wave = h.state().now.waveform.clone().expect("waveform for the playing track");
    assert!(wave.peak.iter().any(|&p| p > 20));
    assert!(!stale.exists(), "unused entry pruned at startup");
    // A slow machine may already have moved on to the next short fixture, so look for this track's entry.
    let fingerprint = crate::store::fingerprint(&played).unwrap();
    assert!(crate::waveform_cache::load(&cache, &fingerprint).is_some(), "the new waveform is cached");
    h.state_mut().player.stop();
}

/// A file played from a rekordbox stick leaves its waveform and its key and BPM in the cache, so the same
/// file played from an ordinary folder later still gets them, and the decoded waveform without the setting.
#[test]
fn rekordbox_waveform_is_cached_for_the_same_file_elsewhere() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let cache = store.paths.waveform_cache();
    let stick = dir.path().join("STICK");
    let copy_dir = |from: &Path, to: &Path| {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap() {
            let e = e.unwrap();
            std::fs::copy(e.path(), to.join(e.file_name())).unwrap();
        }
    };
    let fixture_stick = fixtures().join("rekordbox/PIONEER");
    copy_dir(&fixture_stick.join("rekordbox"), &stick.join("PIONEER/rekordbox"));
    copy_dir(&fixture_stick.join("USBANLZ/P011/00020633"), &stick.join("PIONEER/USBANLZ/P011/00020633"));
    let on_stick = stick.join("Contents/Veracocha/Carte Blanche/Veracocha - Carte Blanche (Original Mix).mp3");
    std::fs::create_dir_all(on_stick.parent().unwrap()).unwrap();
    std::fs::copy(fixtures().join("tagged_v23.mp3"), &on_stick).unwrap();
    let elsewhere = dir.path().join("Music/renamed.mp3");
    std::fs::create_dir_all(elsewhere.parent().unwrap()).unwrap();
    std::fs::copy(&on_stick, &elsewhere).unwrap();

    let mut h = Harness::builder().with_size(egui::vec2(900.0, 600.0)).build_eframe(|cc| App::new(cc, store));
    play_file(&mut h, on_stick.clone());
    let from_stick = h.state().now.waveform.clone().expect("waveform from the stick");
    assert!(h.state().now.wave_colors.is_some(), "rekordbox's colours");
    assert_eq!(from_stick.peak.len(), nullamp_audio::WAVEFORM_BUCKETS, "not the decoded one, which has one bucket per 10 ms at most");
    let fingerprint = crate::store::fingerprint(&on_stick).unwrap();
    assert!(crate::waveform_cache::load_rekordbox(&cache, &fingerprint).is_some(), "stored for the file's content");

    // The stick is gone; the same content elsewhere still has it.
    std::fs::remove_dir_all(&stick).unwrap();
    play_file(&mut h, elsewhere.clone());
    assert_eq!(h.state().now.waveform.as_ref(), Some(&from_stick));
    assert!(h.state().now.wave_colors.is_some());
    // Key and BPM, which the file's tags lack, come from the cached record and reach the playlist row.
    let row = h.state().player.lists[0].playlist.entries().iter().find(|e| e.path == elsewhere).map(|e| e.info.clone()).unwrap();
    assert_eq!((row.bpm, row.key), (Some(140.02), nullamp_core::Key::parse("Bbm")));
    let record = crate::waveform_cache::load_rekordbox_data(&cache, &fingerprint).unwrap();
    assert_eq!((record.key.as_deref(), record.rating, record.color.as_deref()), (Some("Bbm"), 0, None));

    h.state_mut().settings.rekordbox_waveform = false;
    play_file(&mut h, elsewhere);
    assert!(h.state().now.wave_colors.is_none(), "with the setting off the decoded waveform is used");
    h.state_mut().player.stop();
}

/// Play `path` (added to the first list) and let the background work arrive.
fn play_file(h: &mut Harness<'_, App>, path: PathBuf) {
    let app = h.state_mut();
    let id = app.player.lists[0].playlist.add_paths([path])[0];
    app.player.play_entry(0, id, 0.0);
    settle(h, 1500);
}

/// A file without a BPM tag gets its tempo found in the background (and cached, so the next
/// play is instant); one with a BPM tag shows that and is not analysed at all.
#[test]
fn tempo_is_detected_unless_the_tags_have_one() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let cache = store.paths.waveform_cache();
    let mut h = Harness::builder().with_size(egui::vec2(900.0, 600.0)).build_eframe(|cc| App::new(cc, store));

    play_file(&mut h, fixtures().join("beat_120.mp3"));
    let tempo = h.state().now.tempo.expect("tempo for the playing track");
    assert!((tempo.bpm - 120.0).abs() < 0.5, "{tempo:?}");
    assert_eq!(h.state().current_bpm(), Some((tempo.bpm, true)));
    let row_bpm = |h: &Harness<'_, App>, name: &str| {
        let e = h.state().player.lists[0].playlist.entries().iter().find(|e| e.path.file_name().is_some_and(|n| n == name)).unwrap();
        (e.info.bpm, e.info.bpm_detected)
    };
    assert_eq!(row_bpm(&h, "beat_120.mp3"), (Some(tempo.bpm), true), "the playlist row shows it");
    assert!(std::fs::read_dir(&cache).unwrap().filter_map(|e| e.ok()).any(|e| e.path().extension().is_some_and(|x| x == "bpm")), "tempo is cached");

    // Another track clears it: a tone has no beat.
    play_file(&mut h, fixtures().join("tone.flac"));
    assert_eq!(h.state().now.tempo, None);
    assert_eq!(h.state().current_bpm(), None);

    // The tag wins and spares the analysis.
    let tagged = dir.path().join("tagged_beat.mp3");
    std::fs::copy(fixtures().join("beat_120.mp3"), &tagged).unwrap();
    let edit = nullamp_meta::Edit { fields: [(nullamp_meta::Field::Bpm, Some("87".to_owned()))].into(), ..Default::default() };
    nullamp_meta::write(&tagged, nullamp_meta::stamp(&tagged).unwrap(), &edit, &Default::default()).unwrap();
    play_file(&mut h, tagged);
    assert_eq!(h.state().now.tempo, None, "not analysed");
    assert_eq!(h.state().current_bpm(), Some((87.0, false)));
    assert_eq!(row_bpm(&h, "tagged_beat.mp3"), (Some(87.0), false), "the tag, not a detected value");
    assert_eq!(row_bpm(&h, "beat_120.mp3"), (Some(tempo.bpm), true), "the detected value stays");
    h.state_mut().player.stop();
}

/// Copy of a fixture with its key tag set to `key` (as written by whichever program).
fn keyed_copy(dir: &Path, name: &str, key: &str) -> PathBuf {
    let path = dir.join(format!("{key}-{name}").replace(['#', ' '], "_"));
    std::fs::copy(fixtures().join(name), &path).unwrap();
    let edit = nullamp_meta::Edit { fields: [(nullamp_meta::Field::Key, Some(key.to_owned()))].into(), ..Default::default() };
    nullamp_meta::write(&path, nullamp_meta::stamp(&path).unwrap(), &edit, &Default::default()).unwrap();
    path
}

/// The key tag reaches the playlist row, which shows it in the chosen notation. The deck shows none.
#[test]
fn key_shows_in_the_playlist_row() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(900.0, 600.0)).build_eframe(|cc| App::new(cc, store));
    let row_key = |h: &Harness<'_, App>, path: &Path| h.state().player.lists[0].playlist.entries().iter().find(|e| e.path == path).unwrap().info.key;

    let am = keyed_copy(dir.path(), "tone.flac", "Am");
    h.state_mut().player.lists[0].playlist.add_paths([am.clone()]);
    h.state().workers.scan(vec![am.clone()]);
    settle(&mut h, 500);
    assert_eq!(row_key(&h, &am), nullamp_core::Key::parse("8A"), "the scan reads it into the row");
    assert_eq!(row_key(&h, &fixtures().join("tone.ogg")), None, "an untagged file has none");
    assert_eq!(h.state().key_notation(), nullamp_core::KeyNotation::Camelot);
    h.state_mut().settings.camelot = false;
    assert_eq!(h.state().key_notation(), nullamp_core::KeyNotation::Musical);
}

#[test]
#[ignore]
fn render_key_column() {
    render_key_column_as("dark", "");
    render_key_column_as("light", "-light");
}

/// All twelve Camelot numbers, the A and B keys alternating, to judge the wheel colours.
fn render_key_column_as(appearance: &str, suffix: &str) {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), appearance);
    let theme = if appearance == "light" { egui::Theme::Light } else { egui::Theme::Dark };
    let mut h = Harness::builder().with_size(egui::vec2(1000.0, 800.0)).with_pixels_per_point(2.0).with_theme(theme).wgpu().build_eframe(|cc| App::new(cc, store));
    let paths: Vec<PathBuf> = (1..=12).map(|n| keyed_copy(dir.path(), "tone.flac", &format!("{n}{}", if n % 2 == 0 { 'B' } else { 'A' }))).collect();
    {
        let app = h.state_mut();
        app.player.lists[0].playlist.clear();
        app.player.lists[0].playlist.add_paths(paths.clone());
        app.workers.scan(paths);
    }
    settle(&mut h, 800);
    save(&mut h, &format!("07-key-camelot{suffix}.png"));
}

/// Single tracks hide the setlist parts; mixes show them, and ⌘L brings them back for
/// a single track that should get a setlist.
#[test]
fn setlist_hidden_for_single_tracks() {
    use crate::ui::SideTab;

    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(1180.0, 740.0)).build_eframe(|cc| App::new(cc, store));
    let play = |h: &mut Harness<'_, App>, index: usize| {
        let app = h.state_mut();
        let id = app.player.lists[0].playlist.entries()[index].id;
        app.player.play_entry(0, id, 0.0);
        settle(h, 1500);
    };
    h.state_mut().side_tab = SideTab::Setlist;

    play(&mut h, 2);
    assert!(!h.state().mix.is_mix, "tone.flac is a short single track");
    assert_eq!(h.state().shown_tab(), SideTab::Inspector);
    assert_eq!(h.state().side_tab, SideTab::Setlist, "the saved choice is kept for the next mix");

    play(&mut h, 5);
    assert!(h.state().mix.is_mix, "the mix fixture has chapters");
    assert_eq!(h.state().shown_tab(), SideTab::Setlist);

    play(&mut h, 2);
    assert!(!h.state().mix.is_mix);
    h.state_mut().side_open = false;
    h.state_mut().toggle_side(SideTab::Setlist);
    settle(&mut h, 100);
    assert!(h.state().mix.is_mix && h.state().mix.pinned);
    assert_eq!(h.state().shown_tab(), SideTab::Setlist);
    h.state_mut().player.stop();
}

/// The setlist entry editor can be dismissed, with or without saving.
#[test]
fn entry_editor_closes_on_cancel_and_save() {
    use egui_kittest::kittest::Queryable;

    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(1180.0, 740.0)).build_eframe(|cc| App::new(cc, store));
    {
        let app = h.state_mut();
        let id = app.player.lists[0].playlist.entries()[5].id;
        app.player.play_entry(0, id, 0.0);
    }
    settle(&mut h, 1500);
    assert!(h.state().mix.setlist.is_some(), "the mix fixture has chapters");

    crate::ui::select_setlist_entry(h.state_mut(), 1);
    settle(&mut h, 200);
    assert!(h.state().mix.selected.is_some());
    h.get_by_label("Cancel").click();
    settle(&mut h, 200);
    assert!(h.state().mix.selected.is_none(), "cancel closes the editor");

    crate::ui::select_setlist_entry(h.state_mut(), 1);
    settle(&mut h, 200);
    h.get_by_label("Save").click();
    settle(&mut h, 200);
    assert!(h.state().mix.selected.is_none(), "save closes the editor");
    h.state_mut().player.stop();
}

/// The volume meter at several levels and scales, including the 1x and 1.5x common on Linux,
/// where bars that are not pixel aligned come out with uneven widths and gaps.
#[test]
#[ignore]
fn render_volume_meter() {
    for (ppp, name) in [(1.0, "05c-volume-1x.png"), (1.5, "05d-volume-1.5x.png"), (2.0, "05e-volume-2x.png")] {
        let mut h = Harness::builder().with_size(egui::vec2(150.0, 110.0)).with_pixels_per_point(ppp).with_theme(egui::Theme::Dark).build_ui(|ui| {
            // A fractional start, as the meter gets in the transport row.
            ui.add_space(0.3);
            for (level, muted) in [(0.0, false), (0.35, false), (0.8, false), (1.0, false), (0.6, true)] {
                let mut v = level;
                crate::ui::widgets::level_slider(ui, &mut v, 120.0, muted);
            }
        });
        h.run();
        h.render().expect("render").save(out_dir().join(name)).unwrap();
    }
}

/// The output picker opened with a click on the LCD label, and every output icon.
#[test]
#[ignore]
fn render_output_menu() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(1100.0, 420.0)).with_pixels_per_point(2.0).with_theme(egui::Theme::Dark).wgpu().build_eframe(|cc| App::new(cc, store));
    settle(&mut h, 800);
    let at = egui::pos2(830.0, 66.0);
    h.event(egui::Event::PointerMoved(at));
    h.step();
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() });
        h.step();
    }
    settle(&mut h, 200);
    // A name as long as Linux reports for HDMI outputs, which must end in an ellipsis inside the menu.
    h.state_mut().output_devices.push(nullamp_audio::OutputDevice {
        id: "long".into(),
        name: "Ellesmere HDMI Audio [Radeon RX 470/480 / 570/580/590] Digital Stereo (HDMI 3)".into(),
        is_default: false,
        kind: nullamp_audio::OutputKind::Unknown,
    });
    settle(&mut h, 100);
    save(&mut h, "05-output-menu.png");

    use nullamp_audio::OutputKind::*;
    let mut icons = Harness::builder().with_size(egui::vec2(420.0, 70.0)).with_pixels_per_point(3.0).with_theme(egui::Theme::Dark).build_ui(|ui| {
        ui.horizontal(|ui| {
            for kind in [Unknown, BuiltIn, Usb, Bluetooth, Display, Network, Card, Virtual] {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(36.0, 36.0), egui::Sense::hover());
                crate::ui::widgets::paint_output_icon(ui.painter(), rect, kind, ui.visuals().text_color());
            }
        });
    });
    icons.run();
    icons.render().expect("render").save(out_dir().join("05b-output-icons.png")).unwrap();
}

/// Right-clicking the display (on its empty part and on the clock) opens the settings menu, and a
/// menu entry opens its tab.
#[test]
#[ignore]
fn render_settings_menu() {
    use egui_kittest::kittest::Queryable;
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(1180.0, 740.0)).with_pixels_per_point(2.0).with_theme(egui::Theme::Dark).wgpu().build_eframe(|cc| App::new(cc, store));
    settle(&mut h, 300);
    let right_click = |h: &mut Harness<'_, App>, at: egui::Pos2| {
        h.event(egui::Event::PointerMoved(at));
        for pressed in [true, false] {
            h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Secondary, pressed, modifiers: Default::default() });
        }
        settle(h, 300);
    };
    right_click(&mut h, egui::pos2(500.0, 70.0));
    save(&mut h, "06d-settings-menu.png");
    assert!(h.state().dialogs.settings.is_none());
    let entry = h.get_by_label("Keyboard shortcuts").rect().center();
    h.event(egui::Event::PointerMoved(entry));
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: entry, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() });
    }
    settle(&mut h, 300);
    assert_eq!(h.state().dialogs.settings, Some(crate::ui::dialogs::SettingsTab::Shortcuts));
}

/// The General settings page, which takes over the deck.
#[test]
#[ignore]
fn render_settings_general() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(820.0, 520.0)).with_pixels_per_point(2.0).with_theme(egui::Theme::Dark).wgpu().build_eframe(|cc| App::new(cc, store));
    h.state_mut().dialogs.settings = Some(crate::ui::dialogs::SettingsTab::General);
    settle(&mut h, 300);
    save(&mut h, "06a-settings-general.png");
}

/// The rekordbox settings page, which takes over the deck.
#[test]
#[ignore]
fn render_settings_rekordbox() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(820.0, 520.0)).with_pixels_per_point(2.0).with_theme(egui::Theme::Dark).wgpu().build_eframe(|cc| App::new(cc, store));
    h.state_mut().dialogs.settings = Some(crate::ui::dialogs::SettingsTab::Rekordbox);
    settle(&mut h, 300);
    save(&mut h, "06a2-settings-rekordbox.png");
}

/// The shortcuts page, which takes over the deck.
#[test]
#[ignore]
fn render_shortcuts() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(640.0, 520.0)).with_pixels_per_point(2.0).with_theme(egui::Theme::Dark).wgpu().build_eframe(|cc| App::new(cc, store));
    h.state_mut().dialogs.settings = Some(crate::ui::dialogs::SettingsTab::Shortcuts);
    settle(&mut h, 300);
    save(&mut h, "06b-shortcuts.png");
}

/// The audio output picker in the player-only layout, where the menu would not fit below the deck.
#[test]
#[ignore]
fn render_outputs_page() {
    use nullamp_audio::OutputKind::*;
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(820.0, 160.0)).with_pixels_per_point(2.0).with_theme(egui::Theme::Dark).wgpu().build_eframe(|cc| App::new(cc, store));
    h.state_mut().playlist_open = false;
    h.state_mut().side_open = false;
    settle(&mut h, 300);
    let device = |id: &str, kind, is_default| nullamp_audio::OutputDevice { id: id.into(), name: id.into(), kind, is_default };
    let app = h.state_mut();
    app.output_devices = vec![device("MacBook Pro Speakers", BuiltIn, true), device("External Headphones", BuiltIn, false), device("Studio Display Speakers", Display, false), device("AirPods Pro", Bluetooth, false), device("BlackHole 2ch", Virtual, false)];
    app.settings.output_device = Some("AirPods Pro".into());
    app.dialogs.settings = Some(crate::ui::dialogs::SettingsTab::Audio);
    settle(&mut h, 300);
    save(&mut h, "06c-outputs-page.png");
}

/// The About page, taking over the deck.
#[test]
#[ignore]
fn render_about() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(800.0, 520.0)).with_pixels_per_point(2.0).with_theme(egui::Theme::Dark).wgpu().build_eframe(|cc| App::new(cc, store));
    h.state_mut().dialogs.settings = Some(crate::ui::dialogs::SettingsTab::About);
    settle(&mut h, 500);
    save(&mut h, "06-about.png");
}

/// The Logs page with a few log lines.
#[test]
#[ignore]
fn render_logs() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(800.0, 520.0)).with_pixels_per_point(2.0).with_theme(egui::Theme::Dark).wgpu().build_eframe(|cc| App::new(cc, store));
    crate::logbuf::init("info");
    for i in 0..60 {
        log::info!("nullamp: sample log line {i}");
    }
    h.state_mut().dialogs.settings = Some(crate::ui::dialogs::SettingsTab::Logs);
    settle(&mut h, 500);
    save(&mut h, "06d-logs.png");
}

/// The Updates page with an update available.
#[test]
#[ignore]
fn render_updates() {
    use crate::ui::dialogs::UpdateState;
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(800.0, 520.0)).with_pixels_per_point(2.0).with_theme(egui::Theme::Dark).wgpu().build_eframe(|cc| App::new(cc, store));
    let release = nullamp_update::Release { version: "1.2.0".into(), page: Some("https://github.com/jbrunemann/nullamp/releases/tag/v1.2.0".into()) };
    h.state_mut().dialogs.settings = Some(crate::ui::dialogs::SettingsTab::Updates);
    h.state_mut().dialogs.update = UpdateState::Done(Ok(nullamp_update::Status::Available(release)));
    settle(&mut h, 500);
    save(&mut h, "06e-updates.png");
}

/// A library with a folder holding a folder, and loose playlists.
fn rekordbox_overview() -> crate::rekordbox::Overview {
    let item = |id: &str, name: &str, depth, is_folder, tracks| crate::rekordbox::Item { id: id.into(), name: name.into(), depth, is_folder, tracks };
    crate::rekordbox::Overview {
        tracks: 4210,
        items: vec![
            item("c", "Clubs", 0, true, 0),
            item("c1", "Peak time", 1, false, 112),
            item("c2", "Warm up", 1, false, 64),
            item("s", "Summer", 1, true, 0),
            item("s1", "Beach", 2, false, 38),
            item("f", "Favourites", 0, false, 240),
            item("e", "Empty", 0, false, 0),
        ],
    }
}

/// The rekordbox page with and without a Python 3 on the machine.
#[test]
#[ignore]
fn render_rekordbox_tab() {
    use crate::ui::dialogs::PythonState;
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(800.0, 520.0)).with_pixels_per_point(2.0).with_theme(egui::Theme::Dark).wgpu().build_eframe(|cc| App::new(cc, store));
    h.state_mut().dialogs.settings = Some(crate::ui::dialogs::SettingsTab::Rekordbox);
    h.state_mut().dialogs.python = PythonState::Found("3.12.4".into());
    settle(&mut h, 500);
    save(&mut h, "06f-rekordbox-python.png");
    h.state_mut().dialogs.python = PythonState::Missing;
    settle(&mut h, 500);
    save(&mut h, "06g-rekordbox-no-python.png");
}

/// The chooser for the desktop library.
#[test]
#[ignore]
fn render_rekordbox_chooser() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(800.0, 620.0)).with_pixels_per_point(2.0).with_theme(egui::Theme::Dark).wgpu().build_eframe(|cc| App::new(cc, store));
    crate::ui::dialogs::open_rekordbox_import(h.state_mut(), rekordbox_overview());
    h.state_mut().dialogs.rekordbox_import.as_mut().unwrap().selected.extend(["c1".to_owned(), "f".to_owned()]);
    settle(&mut h, 500);
    save(&mut h, "06h-rekordbox-chooser.png");
}

/// Ticking a folder ticks what is inside it; "Add" with nothing ticked is not possible and the
/// dialog closes on Cancel.
#[test]
fn rekordbox_chooser_ticks_folders_and_closes() {
    use egui_kittest::kittest::Queryable;
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(800.0, 620.0)).build_eframe(|cc| App::new(cc, store));
    crate::ui::dialogs::open_rekordbox_import(h.state_mut(), rekordbox_overview());
    settle(&mut h, 200);
    h.get_by_label("Clubs").click();
    settle(&mut h, 200);
    let ticked = |h: &Harness<App>| {
        let mut ids: Vec<String> = h.state().dialogs.rekordbox_import.as_ref().unwrap().selected.iter().cloned().collect();
        ids.sort();
        ids
    };
    assert_eq!(ticked(&h), ["c1", "c2", "s1"]);
    h.get_by_label("Beach").click();
    settle(&mut h, 200);
    assert_eq!(ticked(&h), ["c1", "c2"]);
    h.get_by_label("Cancel").click();
    settle(&mut h, 200);
    assert!(h.state().dialogs.rekordbox_import.is_none());
}

/// The Cache page counts what the cache folder holds, and "Clear all" empties it.
#[test]
fn cache_tab_counts_and_clears() {
    use egui_kittest::kittest::Queryable;
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let cache = store.paths.cache.clone();
    std::fs::create_dir_all(cache.join("waveforms")).unwrap();
    std::fs::write(cache.join("waveforms/0000000000000001.wf"), vec![0u8; 4096]).unwrap();
    std::fs::write(cache.join("waveforms/0000000000000001.bpm"), [0u8; 12]).unwrap();
    let mut h = Harness::builder().with_size(egui::vec2(820.0, 520.0)).build_eframe(|cc| App::new(cc, store));
    h.state_mut().dialogs.settings = Some(crate::ui::dialogs::SettingsTab::Cache);
    settle(&mut h, 500);
    let usage = h.state().dialogs.cache.usage.clone().expect("scanned on opening the tab");
    assert_eq!(usage.iter().map(|(_, u)| u.entries).sum::<usize>(), 2);
    h.get_by_label("Clear all").click();
    settle(&mut h, 500);
    assert!(!cache.join("waveforms/0000000000000001.wf").exists());
    assert!(h.state().dialogs.cache.usage.as_ref().unwrap().iter().all(|(_, u)| u.entries == 0));
}

/// The Cache page with something in every kind.
#[test]
#[ignore]
fn render_cache() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let cache = store.paths.cache.clone();
    for (rel, len) in [
        ("waveforms/1.wf", 4100),
        ("waveforms/2.wf", 4100),
        ("waveforms/1.bpm", 12),
        ("waveforms/1.rwf", 10240),
        ("artwork/tracks/1.jpg", 180_000),
        ("artwork/tracks/2.none", 1),
        ("artwork/now-1.jpg", 420_000),
        ("online/0240/page", 29_000_000),
    ] {
        std::fs::create_dir_all(cache.join(rel).parent().unwrap()).unwrap();
        std::fs::write(cache.join(rel), vec![0u8; len]).unwrap();
    }
    let mut h = Harness::builder().with_size(egui::vec2(820.0, 520.0)).with_pixels_per_point(2.0).with_theme(egui::Theme::Dark).wgpu().build_eframe(|cc| App::new(cc, store));
    h.state_mut().dialogs.settings = Some(crate::ui::dialogs::SettingsTab::Cache);
    settle(&mut h, 500);
    save(&mut h, "06f-cache.png");
}

/// While a mix plays, the deck shows the tempo of the active track, found when it becomes
/// active, not one for the whole file.
#[test]
fn mix_tempo_follows_the_active_track() {
    use nullamp_core::setlist::{Provenance, SetlistEntry};

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("mix.mp3");
    std::fs::copy(fixtures().join("beats_120_90.mp3"), &file).unwrap();
    let paths = AppPaths { config: dir.path().join("config"), data: dir.path().join("data"), cache: dir.path().join("cache") };
    let store = Store::new(paths.clone());
    let mut pl = nullamp_core::Playlist::new("Default");
    pl.add_paths([file.clone()]);
    store.save_playlist(&PlaylistDoc { key: "default".into(), playlist: pl }).unwrap();
    store.save_settings(&crate::store::Settings { volume: 0.0, ..Default::default() }).unwrap();
    let entries = vec![
        SetlistEntry::new("A", "Slow House", Some(0.0), Provenance::Manual),
        SetlistEntry::new("B", "Slower House", Some(15.0), Provenance::Manual),
    ];
    let saved = crate::store::SavedSetlist { fingerprint: crate::store::fingerprint(&file).unwrap(), last_path: file.clone(), setlist: nullamp_core::Setlist { entries, ..Default::default() } };
    crate::store::save_setlist(&paths, &saved.fingerprint, Some(&saved)).unwrap();

    let mut h = Harness::builder().with_size(egui::vec2(900.0, 600.0)).build_eframe(|cc| App::new(cc, store));
    {
        let app = h.state_mut();
        let id = app.player.lists[0].playlist.entries()[0].id;
        app.player.play_entry(0, id, 0.0);
    }
    settle(&mut h, 1500);
    assert!(h.state().mix.is_mix);
    let bpm = h.state().current_bpm().expect("tempo of the first track");
    assert!((bpm.0 - 120.0).abs() < 0.5 && bpm.1, "{bpm:?}");

    h.state_mut().player.seek_to(20.0);
    settle(&mut h, 1000);
    let bpm = h.state().current_bpm().expect("tempo of the second track");
    assert!((bpm.0 - 90.0).abs() < 0.5, "{bpm:?}");
    h.state_mut().player.stop();
}

/// The deck's format line with a detected tempo, and the inspector offering it for the tag.
#[test]
#[ignore]
fn render_tempo() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(1180.0, 740.0)).with_pixels_per_point(2.0).with_theme(egui::Theme::Dark).wgpu().build_eframe(|cc| App::new(cc, store));
    play_file(&mut h, fixtures().join("beat_120.mp3"));
    {
        let app = h.state_mut();
        app.side_tab = crate::ui::SideTab::Inspector;
        let id = app.player.lists[0].playlist.entries().last().unwrap().id;
        app.selection.ids = [id].into();
        app.selection.cursor = Some(id);
        crate::ui::inspect_selection(app);
    }
    settle(&mut h, 1200);
    save(&mut h, "10-tempo.png");

    // A title long enough to run under the tempo box.
    let long = dir.path().join("long.mp3");
    std::fs::copy(fixtures().join("beat_120.mp3"), &long).unwrap();
    let edit = nullamp_meta::Edit { fields: [(nullamp_meta::Field::Title, Some("A very long title that keeps going well past the end of the line, so that it runs right under the tempo box and we can see it fade out".to_owned()))].into(), ..Default::default() };
    nullamp_meta::write(&long, nullamp_meta::stamp(&long).unwrap(), &edit, &Default::default()).unwrap();
    play_file(&mut h, long);
    settle(&mut h, 500);
    save(&mut h, "10-tempo-long-title.png");
    h.state_mut().player.stop();
}

/// The deck artwork: dithered by default, then the original, then off, as a click cycles them.
#[test]
#[ignore]
fn render_deck_retro_artwork() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(1180.0, 740.0)).with_pixels_per_point(2.0).with_theme(egui::Theme::Dark).wgpu().build_eframe(|cc| App::new(cc, store));
    settle(&mut h, 800);
    {
        let app = h.state_mut();
        let id = app.player.lists[0].playlist.entries()[0].id;
        app.player.play_entry(0, id, 0.0);
    }
    settle(&mut h, 1500);
    assert!(h.state().now.art.is_some() && h.state().now.retro_art.is_some(), "the fixture has a cover");
    save(&mut h, "05-deck-art-retro.png");
    h.state_mut().settings.artwork = ArtworkMode::Original;
    settle(&mut h, 200);
    save(&mut h, "05b-deck-art-original.png");
    h.state_mut().settings.artwork = ArtworkMode::Off;
    settle(&mut h, 200);
    save(&mut h, "05c-deck-art-off.png");
    h.state_mut().player.stop();
}

/// The deck at 1x scale, where small text has to be hinted onto whole pixels.
#[test]
#[ignore]
fn render_deck_low_dpi() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(700.0, 200.0)).with_pixels_per_point(1.0).with_theme(egui::Theme::Dark).wgpu().build_eframe(|cc| App::new(cc, store));
    play_file(&mut h, fixtures().join("beat_120.mp3"));
    settle(&mut h, 1200);
    save(&mut h, "11-deck-1x.png");
    h.state_mut().player.stop();
}

#[test]
fn a_failed_cover_lookup_is_not_asked_again_every_frame() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(1180.0, 740.0)).wgpu().build_eframe(|cc| App::new(cc, store));
    {
        let app = h.state_mut();
        app.settings.online_artwork = false;
        app.settings.visualizer = "off".into();
        let id = app.player.lists[0].playlist.entries()[5].id;
        app.player.play_entry(0, id, 0.0);
    }
    settle(&mut h, 1500);
    h.state_mut().player.seek_to(0.2);
    settle(&mut h, 300);

    let (key, _) = h.state().active_entry_key().expect("the mix fixture names its tracks");
    let app = h.state_mut();
    app.now.entry_art.insert(key.clone(), crate::ui::EntryCover::Looking(std::time::Instant::now()));
    app.now.entry_art_active = Some(key.clone());
    app.on_entry_art(key.clone(), Err("no network".into()));
    let crate::ui::EntryCover::Failed(failed_at) = h.state().now.entry_art[&key] else { panic!("an error is remembered as a failure") };

    // Lookups are on again; the track stays active, so nothing is asked until the pause is over.
    h.state_mut().settings.online_artwork = true;
    for _ in 0..5 {
        h.step();
    }
    assert!(matches!(h.state().now.entry_art[&key], crate::ui::EntryCover::Failed(at) if at == failed_at));
    assert!(h.state().deck_art().is_some_and(|a| a.source == crate::ui::ArtSource::File), "the deck keeps the file's cover");
}

/// The deck's cover dissolves into the next file's, also when neither is part of a mix.
#[test]
fn the_deck_cover_changes_between_files_with_a_transition() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(1180.0, 740.0)).wgpu().build_eframe(|cc| App::new(cc, store));
    h.state_mut().settings.visualizer = "off".into();
    play_file(&mut h, fixtures().join("tone.flac"));
    let first = h.state().deck_art().and_then(|a| a.art).map(|t| t.id());
    assert!(first.is_some() && h.state().now.art_transition.is_none(), "the first file shows its placeholder");

    let app = h.state_mut();
    let id = app.player.lists[0].playlist.add_paths([fixtures().join("tagged_v24.mp3")])[0];
    app.player.play_entry(0, id, 0.0);
    let mut started = false;
    for _ in 0..30 {
        std::thread::sleep(Duration::from_millis(50));
        h.step();
        started |= h.state().now.art_transition.is_some();
    }
    assert!(started, "the next file's cover is changed to with a transition");
    assert_ne!(h.state().deck_art().and_then(|a| a.art).map(|t| t.id()), first);
    h.state_mut().player.stop();
}

/// The deck's cover turning from the mix's to a track's, frame by frame, side by side in one strip.
#[test]
#[ignore]
fn render_entry_art_transition() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(1180.0, 740.0)).with_pixels_per_point(2.0).with_theme(egui::Theme::Dark).wgpu().build_eframe(|cc| App::new(cc, store));
    {
        let app = h.state_mut();
        app.settings.online_artwork = false;
        app.settings.visualizer = "off".into();
        let id = app.player.lists[0].playlist.entries()[5].id;
        app.player.play_entry(0, id, 0.0);
    }
    settle(&mut h, 1500);
    // The first track; the fixture's second has no artist to look up.
    h.state_mut().player.seek_to(0.2);
    settle(&mut h, 300);

    // A found cover for the active track, as if it had just arrived.
    let bytes = std::fs::read(fixtures().join("cover.jpg")).unwrap();
    let thumb = crate::workers::decode_thumb(&bytes, 512).unwrap();
    let retro = crate::retro_art::retro_thumb(&thumb);
    let texture = |ctx: &egui::Context, t: &crate::workers::Thumb| ctx.load_texture("test-entry-art", egui::ColorImage::from_rgba_unmultiplied(t.size, &t.rgba), egui::TextureOptions::LINEAR);
    let (key, _) = h.state().active_entry_key().expect("the mix fixture names its tracks");
    let art = crate::ui::EntryArtwork { art: texture(&h.ctx, &thumb), retro_art: retro.as_ref().map(|t| texture(&h.ctx, t)), file: None };
    h.state_mut().now.entry_art.insert(key, crate::ui::EntryCover::Found(art));
    h.step();
    assert!(h.state().now.art_transition.is_some(), "a found cover starts the transition");

    let (art_size, ppp) = (106.0 + 16.0, 2.0);
    let (x, y) = (((1180.0 - art_size) * ppp) as u32, 0);
    let side = (art_size * ppp) as u32;
    let moments = [0.0, 0.12, 0.24, 0.36, 0.48, 0.6, 0.7, 0.8];
    let mut strip = image::RgbaImage::new(side * moments.len() as u32, side);
    // The harness moves its clock on by a fixed step each frame; the transition reads it during the next one.
    let before = h.ctx.input(|i| i.time);
    h.step();
    let frame_step = h.ctx.input(|i| i.time) - before;
    for (i, at) in moments.iter().enumerate() {
        let now = h.ctx.input(|i| i.time) + frame_step;
        if let Some(t) = h.state_mut().now.art_transition.as_mut() {
            t.started = now - at;
        }
        h.step();
        let frame = h.render().expect("render");
        image::imageops::replace(&mut strip, &image::imageops::crop_imm(&frame, x, y, side, side).to_image(), (side * i as u32) as i64, 0);
    }
    strip.save(out_dir().join("12-entry-art-transition.png")).unwrap();
    h.state_mut().player.stop();
}

/// The timeline with a waveform from the fixture stick's rekordbox analysis, in its colours and,
/// for comparison, as the plain tint a decoded waveform gets.
#[test]
#[ignore]
fn render_rekordbox_waveform() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(1180.0, 740.0)).with_pixels_per_point(2.0).with_theme(egui::Theme::Dark).wgpu().build_eframe(|cc| App::new(cc, store));
    let id = h.state().player.lists[0].playlist.entries()[5].id;
    h.state_mut().player.play_entry(0, id, 0.0);
    settle(&mut h, 1500);
    let stick_file = fixtures().join("rekordbox/Contents/Veracocha/Carte Blanche/Veracocha - Carte Blanche (Original Mix).mp3");
    let found = crate::rekordbox::Sticks::default().waveform(&stick_file).expect("fixture waveform");
    h.state_mut().player.seek_to(2.6);
    h.state_mut().now.waveform = Some(found.wave);
    h.state_mut().now.wave_colors = found.colors;
    settle(&mut h, 300);
    save(&mut h, "rekordbox-waveform-colour.png");
    h.state_mut().now.wave_colors = None;
    settle(&mut h, 100);
    save(&mut h, "rekordbox-waveform-plain.png");
    h.state_mut().player.stop();
}
