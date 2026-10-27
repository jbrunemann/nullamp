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

pub(crate) fn seeded_store(dir: &Path, appearance: &str) -> Store {
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

pub(crate) fn settle(h: &mut Harness<'_, App>, ms: u64) {
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
        app.setlist_open = false;
        app.inspector_open = true;
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
fn render_playlist_filter() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(1180.0, 420.0)).with_pixels_per_point(2.0).with_theme(egui::Theme::Dark).wgpu().build_eframe(|cc| App::new(cc, store));
    settle(&mut h, 1200);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::F);
    settle(&mut h, 100);
    h.event(egui::Event::Text("TONE".into()));
    settle(&mut h, 200);
    save(&mut h, "04d-playlist-filter.png");
    let names = |h: &Harness<'_, App>| {
        let app = h.state();
        let mut v: Vec<String> = app.player.lists[0].playlist.entries().iter().filter(|e| app.selection.ids.contains(&e.id)).map(|e| e.path.file_name().unwrap().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    };
    // Enter leaves the field with the first match selected; ⌘A then takes only the matches.
    h.key_press(egui::Key::Enter);
    settle(&mut h, 100);
    assert_eq!(names(&h), ["tagged_v23.mp3"]);
    h.key_press(egui::Key::End);
    settle(&mut h, 100);
    assert_eq!(names(&h), ["tone_mono_22k.wav"], "the last shown row, not the last entry");
    h.key_press(egui::Key::ArrowUp);
    settle(&mut h, 100);
    assert_eq!(names(&h), ["tone.m4a"], "arrows skip hidden rows");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    settle(&mut h, 100);
    // The titles of the tagged files and the file names of the rest contain "tone"; the mix and the missing file do not.
    assert_eq!(names(&h), ["tagged_v23.mp3", "tagged_v24.mp3", "tone.flac", "tone.m4a", "tone.ogg", "tone_mono_22k.wav"]);
    // Esc in the field clears the filter.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::F);
    settle(&mut h, 100);
    h.key_press(egui::Key::Escape);
    settle(&mut h, 100);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    settle(&mut h, 100);
    assert_eq!(names(&h).len(), 8);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::F);
    settle(&mut h, 100);
    h.event(egui::Event::Text("nothing like this".into()));
    settle(&mut h, 200);
    save(&mut h, "04e-playlist-filter-none.png");
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
        app.setlist_open = false;
        app.inspector_open = false;
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
    h.state_mut().settings.rekordbox_waveform = true;
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
    let decoded = h.state().now.waveform.clone().expect("decoded waveform");
    assert_ne!(decoded.peak.len(), nullamp_audio::WAVEFORM_BUCKETS, "with the setting off the decoded waveform is used");
    assert!(h.state().now.wave_colors.is_none(), "the default dithered artwork does not ask for band colours");
    h.state_mut().player.stop();
}

/// Play `path` (added to the first list) and let the background work arrive.
fn play_file(h: &mut Harness<'_, App>, path: PathBuf) {
    let app = h.state_mut();
    let id = app.player.lists[0].playlist.add_paths([path])[0];
    app.player.play_entry(0, id, 0.0);
    settle(h, 1500);
}

/// Band colours are only computed while the original artwork is chosen. A waveform decoded without them
/// is decoded again, and its cache entry upgraded, when the original artwork is chosen later; an entry
/// with colours is used under any setting and never written back without them.
#[test]
fn band_colours_are_computed_when_the_original_artwork_asks_for_them() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let cache = store.paths.waveform_cache();
    let mut h = Harness::builder().with_size(egui::vec2(900.0, 600.0)).build_eframe(|cc| App::new(cc, store));
    let file = dir.path().join("Music/band.mp3");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::copy(fixtures().join("beats_120_90.mp3"), &file).unwrap();
    let fingerprint = crate::store::fingerprint(&file).unwrap();

    play_file(&mut h, file.clone());
    assert!(h.state().now.waveform.is_some());
    assert!(h.state().now.wave_colors.is_none(), "dithered artwork: no colours asked for");
    assert!(crate::waveform_cache::load(&cache, &fingerprint).unwrap().colors.is_none());

    h.state_mut().settings.artwork = ArtworkMode::Original;
    h.state_mut().request_wave_colors();
    settle(&mut h, 1000);
    assert!(h.state().now.wave_colors.is_some(), "the playing track's colours arrive");
    assert!(crate::waveform_cache::load(&cache, &fingerprint).unwrap().colors.is_some(), "the entry was upgraded");

    h.state_mut().settings.artwork = ArtworkMode::Dithered;
    play_file(&mut h, file);
    assert!(crate::waveform_cache::load(&cache, &fingerprint).unwrap().colors.is_some(), "not written back without colours");
    assert!(h.state().now.waveform.is_some());
    h.state_mut().player.stop();
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
    use crate::ui::sections::Section;

    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(1180.0, 740.0)).build_eframe(|cc| App::new(cc, store));
    let play = |h: &mut Harness<'_, App>, index: usize| {
        let app = h.state_mut();
        let id = app.player.lists[0].playlist.entries()[index].id;
        app.player.play_entry(0, id, 0.0);
        settle(h, 1500);
    };
    h.state_mut().setlist_open = true;

    play(&mut h, 2);
    assert!(!h.state().mix.is_mix, "tone.flac is a short single track");
    assert!(!h.state().plan.setlist);
    assert!(h.state().setlist_open, "the switch is kept for the next mix");

    play(&mut h, 5);
    assert!(h.state().mix.is_mix, "the mix fixture has chapters");
    assert!(h.state().plan.setlist);

    play(&mut h, 2);
    assert!(!h.state().mix.is_mix);
    h.state_mut().setlist_open = false;
    h.state_mut().toggle_side(Section::Setlist);
    settle(&mut h, 100);
    assert!(h.state().mix.is_mix && h.state().mix.pinned);
    assert!(h.state().plan.setlist);
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

/// The Web settings page, which takes over the deck.
#[test]
#[ignore]
fn render_settings_web() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(820.0, 520.0)).with_pixels_per_point(2.0).with_theme(egui::Theme::Dark).wgpu().build_eframe(|cc| App::new(cc, store));
    h.state_mut().dialogs.settings = Some(crate::ui::dialogs::SettingsTab::Web);
    settle(&mut h, 300);
    save(&mut h, "06f-settings-web.png");
}

/// The Telegram settings page in each step of the sign-in, which takes over the deck.
#[test]
#[ignore]
fn render_settings_telegram() {
    use crate::ui::telegram::{Account, Step};
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(820.0, 520.0)).with_pixels_per_point(2.0).with_theme(egui::Theme::Dark).wgpu().build_eframe(|cc| App::new(cc, store));
    h.state_mut().dialogs.settings = Some(crate::ui::dialogs::SettingsTab::Telegram);
    settle(&mut h, 300);
    save(&mut h, "06g-settings-telegram-empty.png");
    {
        let s = &mut h.state_mut().settings;
        (s.telegram_api_id, s.telegram_api_hash, s.telegram_phone) = ("12345678".into(), "0123456789abcdef0123456789abcdef".into(), "+49 170 1234567".into());
    }
    h.state_mut().telegram.step = Step::Code;
    h.state_mut().telegram.error = Some("Telegram did not accept that code".into());
    settle(&mut h, 300);
    save(&mut h, "06h-settings-telegram-code.png");
    h.state_mut().telegram.step = Step::Idle;
    h.state_mut().telegram.error = None;
    h.state_mut().telegram.account = Account::SignedIn("Jan Brünemann".into());
    settle(&mut h, 300);
    save(&mut h, "06i-settings-telegram-signed-in.png");
}

/// The list of results a Telegram bot offered, for the user to choose from.
#[test]
#[ignore]
fn render_telegram_chooser() {
    use nullamp_telegram::{Choice, Event, Progress};
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(820.0, 640.0)).with_pixels_per_point(2.0).with_theme(egui::Theme::Dark).wgpu().build_eframe(|cc| App::new(cc, store));
    let entry = h.state().player.lists[0].playlist.entries()[0].id;
    let app = h.state_mut();
    app.settings.telegram_bot = "inreach_to_telegram_bot".into();
    app.telegram.asking = Some(crate::ui::telegram::Asking { request: 1, entry, query: "Locklead - Hunting".into() });
    let choice = |title: &str, description: &str, kind: &str, file: Option<&str>| Choice { title: title.into(), description: description.into(), kind: kind.into(), file: file.map(Into::into) };
    let choices = vec![
        choice("Hunting", "Artist: Locklead Album: Square One", "audio", Some("MP3, 9.8 MB, 4:07")),
        choice("Hunting (Extended Mix)", "Artist: Locklead Album: Square One (Remixes)", "audio", Some("FLAC, 31.2 MB, 6:21")),
        choice("Square One (Album)", "Locklead", "article", None),
    ];
    crate::ui::telegram::on_event(app, Event::Ask { request: 1, progress: Progress::Choose(choices) });
    // Two of the three pictures have arrived: made-up covers, the second not square.
    let cover = |w: usize, h: usize, hue: u8| crate::workers::Thumb { size: [w, h], rgba: (0..w * h).flat_map(|i| [hue.wrapping_add((i % w) as u8 * 2), 90, (i / w) as u8 * 3, 255]).collect() };
    crate::ui::telegram::on_thumb(app, 1, 0, cover(64, 64, 20));
    crate::ui::telegram::on_thumb(app, 1, 1, cover(96, 54, 140));
    crate::ui::telegram::on_thumb(app, 9, 2, cover(64, 64, 0));
    settle(&mut h, 300);
    save(&mut h, "06j-telegram-chooser.png");
    // Nothing found: the query is there to edit.
    h.state_mut().telegram.asking = Some(crate::ui::telegram::Asking { request: 2, entry, query: "Locklead - Huntin".into() });
    crate::ui::telegram::on_event(h.state_mut(), Event::Ask { request: 2, progress: Progress::Done(Ok(nullamp_telegram::Outcome::NoResults)) });
    settle(&mut h, 300);
    save(&mut h, "06k-telegram-no-results.png");
}

/// Importing a Spotify playlist: the dialog, and the list of tracks without files it makes.
#[test]
#[ignore]
fn render_spotify_import() {
    use nullamp_spotify::{Playlist, Track};
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(980.0, 640.0)).with_pixels_per_point(2.0).with_theme(egui::Theme::Dark).wgpu().build_eframe(|cc| App::new(cc, store));
    crate::ui::spotify::open(h.state_mut());
    h.state_mut().dialogs.spotify.as_mut().unwrap().link = "https://open.spotify.com/playlist/37i9dQZF1DXcBWIGoYBM5M?si=0a1b2c".into();
    settle(&mut h, 300);
    save(&mut h, "06l-spotify-import.png");
    let import = h.state_mut().dialogs.spotify.as_mut().unwrap();
    import.error = Some("Spotify has no public playlist with this link; it may be private".into());
    settle(&mut h, 300);
    save(&mut h, "06m-spotify-error.png");
    let track = |artist: &str, title: &str, seconds: f64| Track { artist: artist.into(), title: title.into(), duration: Some(seconds), uri: String::new() };
    let tracks = vec![
        track("Fleetwood Mac", "Dreams - 2004 Remaster", 257.0),
        track("Led Zeppelin", "Stairway to Heaven - Remaster", 483.0),
        track("Queen, David Bowie", "Under Pressure - Remastered 2011", 248.0),
        track("Creedence Clearwater Revival", "Bad Moon Rising", 142.0),
    ];
    let app = h.state_mut();
    app.dialogs.spotify.as_mut().unwrap().request = Some(1);
    crate::ui::spotify::on_playlist(app, 1, Ok(Playlist { name: "Rock Classics".into(), id: String::new(), tracks, cut: false }));
    settle(&mut h, 600);
    save(&mut h, "06n-spotify-playlist.png");
}

/// The Change file dialog of a playlist row, with a path that is no file.
#[test]
#[ignore]
fn render_change_file() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(980.0, 640.0)).with_pixels_per_point(2.0).with_theme(egui::Theme::Dark).wgpu().build_eframe(|cc| App::new(cc, store));
    let id = h.state().player.lists[0].playlist.entries()[6].id;
    crate::ui::dialogs::change_file(h.state_mut(), 0, id);
    settle(&mut h, 300);
    save(&mut h, "06o-change-file.png");
}

/// The Change file dialog after searching a folder that holds several files of that name.
#[test]
#[ignore]
fn render_change_file_search() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(980.0, 640.0)).with_pixels_per_point(2.0).with_theme(egui::Theme::Dark).wgpu().build_eframe(|cc| App::new(cc, store));
    let id = h.state().player.lists[0].playlist.entries()[6].id;
    let name = h.state().player.lists[0].playlist.get(id).unwrap().path.file_name().unwrap().to_owned();
    let stem = std::path::Path::new(&name).file_stem().unwrap().to_string_lossy().into_owned();
    let music = dir.path().join("Music");
    for rel in [format!("Old library/{}", name.to_string_lossy()), format!("Backup/2019/{}", name.to_string_lossy()), format!("Converted/{stem}.flac"), format!("Downloads/01 - {stem}.m4a")] {
        let p = music.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"x").unwrap();
    }
    crate::ui::dialogs::change_file(h.state_mut(), 0, id);
    crate::ui::dialogs::on_dialog_result(h.state_mut(), crate::ui::dialogs::DialogResult::ChangeFileFolder(music));
    settle(&mut h, 600);
    save(&mut h, "06o2-change-file-search.png");
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
    h.state_mut().setlist_open = false;
    h.state_mut().inspector_open = false;
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
    crate::ui::dialogs::open_library_import(h.state_mut(), crate::ui::dialogs::ImportSource::Rekordbox, rekordbox_overview());
    h.state_mut().dialogs.library_import.as_mut().unwrap().selected.extend(["c1".to_owned(), "f".to_owned()]);
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
    crate::ui::dialogs::open_library_import(h.state_mut(), crate::ui::dialogs::ImportSource::Rekordbox, rekordbox_overview());
    settle(&mut h, 200);
    h.get_by_label("Clubs").click();
    settle(&mut h, 200);
    let ticked = |h: &Harness<App>| {
        let mut ids: Vec<String> = h.state().dialogs.library_import.as_ref().unwrap().selected.iter().cloned().collect();
        ids.sort();
        ids
    };
    assert_eq!(ticked(&h), ["c1", "c2", "s1"]);
    h.get_by_label("Beach").click();
    settle(&mut h, 200);
    assert_eq!(ticked(&h), ["c1", "c2"]);
    h.get_by_label("Cancel").click();
    settle(&mut h, 200);
    assert!(h.state().dialogs.library_import.is_none());
}

/// The chooser for a Mixxx library, shaped like a real one: three crates, Auto DJ and dated sessions.
fn mixxx_overview() -> crate::rekordbox::Overview {
    use crate::rekordbox::{Item, Overview};
    let item = |id: &str, name: &str, depth: usize, is_folder: bool, tracks: usize| Item { id: id.into(), name: name.into(), depth, is_folder, tracks };
    Overview {
        tracks: 915,
        items: vec![
            item("crates", "Crates", 0, true, 0),
            item("crates/1", "Jan 123", 1, false, 3),
            item("crates/2", "Juni", 1, false, 3),
            item("crates/3", "Trance", 1, false, 30),
            item("playlists", "Playlists", 0, true, 0),
            item("playlists/1", "Auto DJ", 1, false, 3),
            item("history", "History", 0, true, 0),
            item("history/7", "2019-06-28 (2)", 1, false, 4),
            item("history/15", "2019-07-07", 1, false, 9),
        ],
    }
}

/// The Mixxx chooser.
#[test]
#[ignore]
fn render_mixxx_chooser() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(800.0, 620.0)).with_pixels_per_point(2.0).with_theme(egui::Theme::Dark).wgpu().build_eframe(|cc| App::new(cc, store));
    crate::ui::dialogs::open_library_import(h.state_mut(), crate::ui::dialogs::ImportSource::Mixxx, mixxx_overview());
    h.state_mut().dialogs.library_import.as_mut().unwrap().selected.extend(["crates/3".to_owned(), "playlists/1".to_owned()]);
    settle(&mut h, 500);
    save(&mut h, "06i-mixxx-chooser.png");
}

/// The Mixxx chooser says Mixxx, offers the same bulk read as rekordbox's, and ticking a kind ticks its lists.
#[test]
fn mixxx_chooser_ticks_a_kind_of_list() {
    use egui_kittest::kittest::Queryable;
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(800.0, 620.0)).build_eframe(|cc| App::new(cc, store));
    crate::ui::dialogs::open_library_import(h.state_mut(), crate::ui::dialogs::ImportSource::Mixxx, mixxx_overview());
    settle(&mut h, 200);
    assert!(h.query_by_label("Add from Mixxx").is_some());
    assert!(h.query_by_label("Entire library (915 tracks)").is_some());
    assert!(h.query_by_label("Also read waveforms, keys and ratings").is_some());
    h.get_by_label("Crates").click();
    settle(&mut h, 200);
    let mut ids: Vec<String> = h.state().dialogs.library_import.as_ref().unwrap().selected.iter().cloned().collect();
    ids.sort();
    assert_eq!(ids, ["crates/1", "crates/2", "crates/3"]);
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

/// A mono 16-bit WAV of `seconds` of triads in the key of `tonic` (C = 0), with a bass drum on every beat of `bpm`
/// when given, for the key and the tempo to be found in.
fn synth_track(path: &Path, tonic: u8, minor: bool, bpm: Option<f64>, seconds: f64) {
    const RATE: f64 = 22_050.0;
    let third = if minor { 3 } else { 4 };
    let chords: [(i32, i32); 4] = [(0, third), (5, 4), (7, 4), (0, third)];
    let freq = |semitone: i32| 440.0 * 2f64.powf((f64::from(tonic) + 48.0 + f64::from(semitone) - 69.0) / 12.0);
    let total = (seconds * RATE) as usize;
    let tau = std::f64::consts::TAU;
    let samples: Vec<i16> = (0..total)
        .map(|i| {
            let t = i as f64 / RATE;
            let (root, interval) = chords[(i * chords.len() / total).min(chords.len() - 1)];
            let mut v = 0.0;
            for (note, weight) in [(root, 1.0), (root + interval, 0.8), (root + 7, 0.9)] {
                for h in 1..=4 {
                    v += weight / f64::from(h) * (tau * freq(note) * f64::from(h) * t).sin();
                }
            }
            v *= 0.08;
            if let Some(bpm) = bpm {
                let since = t % (60.0 / bpm);
                v += 0.6 * (-since * 30.0).exp() * (tau * 55.0 * since).sin();
            }
            (v.clamp(-1.0, 1.0) * 30_000.0) as i16
        })
        .collect();
    let data = samples.len() as u32 * 2;
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&[1, 0, 1, 0]);
    wav.extend_from_slice(&(RATE as u32).to_le_bytes());
    wav.extend_from_slice(&(RATE as u32 * 2).to_le_bytes());
    wav.extend_from_slice(&[2, 0, 16, 0]);
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data.to_le_bytes());
    wav.extend(samples.iter().flat_map(|s| s.to_le_bytes()));
    std::fs::write(path, wav).unwrap();
}

/// Add `path` to the first list and open it in the inspector without playing it.
fn inspect_file(h: &mut Harness<'_, App>, path: PathBuf) {
    let app = h.state_mut();
    app.setlist_open = false;
    app.inspector_open = true;
    let id = app.player.lists[0].playlist.add_paths([path])[0];
    app.selection.ids = [id].into();
    app.selection.cursor = Some(id);
    crate::ui::inspect_selection(app);
    settle(h, 1500);
}

/// A file without a key tag gets its key found when the inspector opens it, and offers it beside the
/// field; a file that has one is left alone, and the result is cached so the next look is instant.
#[test]
fn inspector_offers_the_detected_key_for_a_file_without_one() {
    use egui_kittest::kittest::Queryable;
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let cache = store.paths.waveform_cache();
    let mut h = Harness::builder().with_size(egui::vec2(1180.0, 740.0)).with_accessibility_check(false).build_eframe(|cc| App::new(cc, store));
    let keys_cached = || std::fs::read_dir(&cache).map(|d| d.filter_map(|e| e.ok()).filter(|e| e.path().extension().is_some_and(|x| x == "key")).count()).unwrap_or(0);

    let plain = dir.path().join("plain.wav");
    synth_track(&plain, 9, true, None, 12.0);
    inspect_file(&mut h, plain.clone());
    h.get_by_label("Detected 8A: use it").click();
    h.run();
    assert_eq!(h.state().inspector.field_value(nullamp_meta::Field::Key), "8A", "the button fills the field");
    assert_eq!(keys_cached(), 1, "the key is cached");

    // The same file again is answered from the cache, with the same offer.
    h.state_mut().inspector_open = false;
    inspect_file(&mut h, plain);
    assert!(h.query_by_label("Detected 8A: use it").is_some());

    // A file with a key tag is not analysed and offers nothing.
    let tagged = dir.path().join("tagged.wav");
    synth_track(&tagged, 2, false, None, 12.0);
    let edit = nullamp_meta::Edit { fields: [(nullamp_meta::Field::Key, Some("Em".to_owned()))].into(), ..Default::default() };
    nullamp_meta::write(&tagged, nullamp_meta::stamp(&tagged).unwrap(), &edit, &Default::default()).unwrap();
    inspect_file(&mut h, tagged);
    assert!(h.query_by_label("Detected 10B: use it").is_none() && h.query_by_label("Detected 5A: use it").is_none(), "the tag is the user's word");
    assert_eq!(keys_cached(), 1, "nothing was analysed for it");
}

/// Key and tempo suggestions beside their short fields, at the panel's usual width and narrower.
#[test]
#[ignore]
fn render_inspector_suggestions() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("Nightdrive.wav");
    synth_track(&file, 9, true, Some(124.0), 20.0);
    for (width, name) in [(1180.0, "wide"), (900.0, "medium"), (440.0, "narrow"), (360.0, "tiny")] {
        let store = seeded_store(dir.path(), "dark");
        let mut h = Harness::builder().with_size(egui::vec2(width, 740.0)).with_pixels_per_point(2.0).with_theme(egui::Theme::Dark).wgpu().build_eframe(|cc| App::new(cc, store));
        play_file(&mut h, file.clone());
        {
            let app = h.state_mut();
            app.setlist_open = false;
            app.inspector_open = true;
            let id = app.player.lists[0].playlist.entries().last().unwrap().id;
            app.selection.ids = [id].into();
            app.selection.cursor = Some(id);
            crate::ui::inspect_selection(app);
        }
        settle(&mut h, 1500);
        // In a narrow window the inspector replaces the playlist once it is asked for.
        if !h.state().plan.inspector {
            h.state_mut().toggle_side(crate::ui::sections::Section::Inspector);
            settle(&mut h, 300);
        }
        save(&mut h, &format!("10-inspector-suggestions-{name}.png"));
        h.state_mut().player.stop();
    }
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
        app.setlist_open = false;
        app.inspector_open = true;
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

/// The timeline with the waveform of a decoded file in each artwork mode: band colours with the
/// original cover, the muted tint with the dithered one, and no waveform with the artwork off.
#[test]
#[ignore]
fn render_waveform_by_artwork_mode() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(1180.0, 740.0)).with_pixels_per_point(2.0).with_theme(egui::Theme::Dark).wgpu().build_eframe(|cc| App::new(cc, store));
    let id = h.state().player.lists[0].playlist.entries()[5].id;
    h.state_mut().player.play_entry(0, id, 0.0);
    settle(&mut h, 1500);
    // NULLAMP_WAVE_FILE draws another file, such as a longer one made for the purpose.
    let file = std::env::var_os("NULLAMP_WAVE_FILE").map(PathBuf::from).unwrap_or_else(|| fixtures().join("beats_120_90.mp3"));
    let mut wave = nullamp_audio::compute_waveform(&file, true, &|| false).expect("fixture waveform");
    h.state_mut().player.seek_to(2.6);
    h.state_mut().now.wave_colors = wave.colors.take();
    h.state_mut().now.waveform = Some(wave);
    for (mode, name) in [(ArtworkMode::Original, "original"), (ArtworkMode::Dithered, "dithered"), (ArtworkMode::Off, "off")] {
        h.state_mut().settings.artwork = mode;
        settle(&mut h, 300);
        save(&mut h, &format!("waveform-artwork-{name}.png"));
    }
    h.state_mut().player.stop();
}

/// As the window narrows the setlist and the inspector first share a column, then the section opened last
/// stands alone, and widening again brings everything back.
#[test]
fn sections_give_way_as_the_window_narrows() {
    use crate::ui::sections::Section;
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(1500.0, 600.0)).build_eframe(|cc| App::new(cc, store));
    {
        let app = h.state_mut();
        app.playlist_open = true;
        app.setlist_open = true;
        app.inspector_open = true;
        let id = app.player.lists[0].playlist.entries()[5].id;
        app.player.play_entry(0, id, 0.0);
    }
    settle(&mut h, 1500);
    assert!(h.state().mix.is_mix, "the mix fixture has chapters");
    let p = h.state().plan;
    assert!(p.playlist && p.setlist && p.inspector && !p.tabs, "all three side by side");

    h.set_size(egui::vec2(900.0, 600.0));
    settle(&mut h, 300);
    let p = h.state().plan;
    assert!(p.playlist && p.tabs && p.setlist != p.inspector, "one column for the setlist and the inspector");

    h.set_size(egui::vec2(640.0, 600.0));
    settle(&mut h, 300);
    let p = h.state().plan;
    assert!(p.playlist && !p.setlist && !p.inspector, "the playlist alone");
    assert!(h.state().setlist_open && h.state().inspector_open, "what was open stays open");

    h.state_mut().toggle_side(Section::Inspector);
    settle(&mut h, 100);
    let p = h.state().plan;
    assert!(p.inspector && !p.playlist, "opening the inspector replaces the playlist");

    h.state_mut().toggle_playlist();
    settle(&mut h, 100);
    let p = h.state().plan;
    assert!(p.playlist && !p.inspector, "opening the playlist replaces the inspector");

    h.set_size(egui::vec2(1500.0, 600.0));
    settle(&mut h, 300);
    let p = h.state().plan;
    assert!(p.playlist && p.setlist && p.inspector, "a wide window brings everything back");
    h.state_mut().player.stop();
}

/// A wide window keeps the playlist and the inspector side by side.
#[test]
fn wide_window_keeps_both_bottom_sections() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(1180.0, 600.0)).build_eframe(|cc| App::new(cc, store));
    h.state_mut().inspector_open = true;
    settle(&mut h, 300);
    assert!(h.state().plan.playlist && h.state().plan.inspector);
}

/// The playlist, the setlist and the inspector side by side, and the setlist and inspector sharing a column.
#[test]
#[ignore = "renders PNGs"]
fn render_three_sections() {
    for (width, name) in [(1500.0, "13-three-sections.png"), (950.0, "13-two-sections-tabbed.png")] {
        let dir = tempfile::tempdir().unwrap();
        let store = seeded_store(dir.path(), "dark");
        let mut h = Harness::builder().with_size(egui::vec2(width, 740.0)).with_pixels_per_point(2.0).with_theme(egui::Theme::Dark).wgpu().build_eframe(|cc| App::new(cc, store));
        {
            let app = h.state_mut();
            app.setlist_open = true;
            app.inspector_open = true;
            let id = app.player.lists[0].playlist.entries()[5].id;
            app.player.play_entry(0, id, 0.0);
        }
        settle(&mut h, 1500);
        save(&mut h, name);
        h.state_mut().player.stop();
    }
}

#[test]
#[ignore]
fn render_narrow_windows() {
    // A phone (1080 px wide at 3x) and the narrowest a Retina window can now be.
    for (width, ppp, name) in [(360.0, 3.0, "phone"), (540.0, 2.0, "narrow")] {
        let dir = tempfile::tempdir().unwrap();
        let store = seeded_store(dir.path(), "dark");
        let mut h = Harness::builder().with_size(egui::vec2(width, 780.0)).with_pixels_per_point(ppp).with_theme(egui::Theme::Dark).wgpu().build_eframe(|cc| App::new(cc, store));
        settle(&mut h, 800);
        save(&mut h, &format!("30-{name}-idle.png"));
        {
            let app = h.state_mut();
            let id = app.player.lists[0].playlist.entries()[5].id;
            app.player.play_entry(0, id, 0.0);
        }
        settle(&mut h, 1500);
        h.state_mut().player.seek_to(2.6);
        settle(&mut h, 800);
        save(&mut h, &format!("31-{name}-mix.png"));
        h.state_mut().toggle_side(crate::ui::sections::Section::Setlist);
        settle(&mut h, 300);
        save(&mut h, &format!("32-{name}-setlist.png"));
        h.state_mut().player.stop();
    }
}

/// The in-window file browser (what Android uses instead of a file dialog), at phone size.
#[test]
#[ignore]
fn render_file_browser() {
    let dir = tempfile::tempdir().unwrap();
    let music = dir.path().join("Music");
    for sub in ["Albums", "Mixes", "Podcasts"] {
        std::fs::create_dir_all(music.join(sub)).unwrap();
    }
    for name in ["01 Opening.mp3", "02 A Very Long Track Name That Does Not Fit On One Line.flac", "cover.jpg", "notes.txt"] {
        std::fs::write(music.join(name), b"x").unwrap();
    }
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(360.0, 780.0)).with_pixels_per_point(3.0).with_theme(egui::Theme::Dark).wgpu().build_eframe(|cc| App::new(cc, store));
    settle(&mut h, 500);
    let (reply, answer) = crossbeam_channel::bounded(1);
    crate::ui::file_browser::submit(crate::ui::file_browser::Request {
        title: "Add audio files".into(),
        extensions: ["mp3", "flac", "m3u"].map(String::from).to_vec(),
        dir: Some(music.clone()),
        name: String::new(),
        mode: crate::ui::file_browser::Mode::Files { multi: true },
        reply,
    });
    settle(&mut h, 800);
    save(&mut h, "40-file-browser.png");
    h.state_mut().dialogs.browser.take();
    drop(answer);
    let (reply, _answer) = crossbeam_channel::bounded(1);
    crate::ui::file_browser::submit(crate::ui::file_browser::Request {
        title: "Export playlist".into(),
        extensions: ["m3u8".to_owned()].to_vec(),
        dir: Some(music),
        name: "Default.m3u8".into(),
        mode: crate::ui::file_browser::Mode::Save,
        reply,
    });
    settle(&mut h, 800);
    save(&mut h, "41-file-browser-save.png");
}

/// The clock below an hour, just past it, counting down, and at its limit. The fixture is seconds long, so
/// the stream's duration is overridden and the position is pinned.
#[test]
#[ignore]
fn render_clock_hours() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(1180.0, 300.0)).with_pixels_per_point(2.0).with_theme(egui::Theme::Dark).wgpu().build_eframe(|cc| App::new(cc, store));
    play_file(&mut h, fixtures().join("beat_120.mp3"));
    // (position, duration, counting down, name)
    for (secs, duration, remaining, name) in [(261.0, 40_000.0, false, "a"), (72.0 * 60.0, 40_000.0, false, "b"), (9.0 * 3600.0 + 59.0 * 60.0 + 59.0, 40_000.0, false, "c"), (4.0 * 60.0 + 21.0, 25.0 * 60.0, true, "d"), (72.0 * 60.0, 40_000.0, true, "e")] {
        h.state_mut().player.current.as_mut().unwrap().info.as_mut().unwrap().duration = Some(duration);
        h.state_mut().show_remaining = remaining;
        // The engine's own events keep resetting the pending position, so pin it right before the render.
        h.state_mut().player.pending_position = Some(secs);
        h.step();
        h.state_mut().player.pending_position = Some(secs);
        save(&mut h, &format!("12-clock-{name}.png"));
    }
    h.state_mut().player.stop();
}
