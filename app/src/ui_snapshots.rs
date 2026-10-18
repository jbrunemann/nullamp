//! Offscreen renders of the real UI for visual review.
//! Requires a GPU (wgpu): cargo test -p nullamp ui_snapshots -- --ignored
//! Images are written to target/screenshots/. Add `--test-threads=1`: the theme
//! palette is process-global, so the dark and light renders must not overlap.

use std::path::{Path, PathBuf};
use std::time::Duration;

use egui_kittest::Harness;

use crate::paths::AppPaths;
use crate::store::{PlaylistDoc, Store};
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
    let settings = crate::store::Settings { volume: 0.0, online_lookups: false, appearance: appearance.into(), ..Default::default() };
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

    {
        let app = h.state_mut();
        app.side_tab = crate::ui::SideTab::Inspector;
        crate::ui::inspect_selection(app);
    }
    settle(&mut h, 1200);
    save(&mut h, &format!("03-inspector{suffix}.png"));
    h.state_mut().player.stop();
}

#[test]
#[ignore]
fn render_player_only() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path(), "dark");
    let mut h = Harness::builder().with_size(egui::vec2(1100.0, 260.0)).with_pixels_per_point(2.0).wgpu().build_eframe(|cc| App::new(cc, store));
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
    save(&mut h, "04-player-only.png");
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
