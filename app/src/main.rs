//! Nullamp: a retro-inspired music and DJ mix player.

// Release builds on Windows are GUI-subsystem programs so no console window opens behind the player.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod cache;
mod datapack;
mod entry_art;
#[cfg(target_os = "macos")]
mod finder;
mod instance;
mod locate;
mod logbuf;
mod mixxx;
mod open;
mod paths;
mod placeholder_art;
mod player;
mod python;
mod rekordbox;
mod retro_art;
mod store;
mod ui;
mod waveform_cache;
mod workers;

#[cfg(test)]
mod ui_snapshots;

fn main() -> eframe::Result {
    // lofty warns on every UTF-8 frame it rewrites as UTF-16 when saving ID3v2.3; the output is correct.
    // symphonia warns about a missing bit reservoir and resync junk after coarse MP3 seeks; a frame is dropped, nothing audible.
    logbuf::init("warn,lofty::util::text=error,lofty::mpeg::properties=error,symphonia_bundle_mp3::layer3=error,symphonia_bundle_mp3::demuxer=error,nullamp=info,nullamp_audio=info");
    let paths = paths::AppPaths::detect();
    log::info!("config: {}", paths.config.display());
    // A second launch hands its files to the running instance and leaves.
    let args = open::args_paths(std::env::args_os().skip(1));
    let listener = match instance::claim(&paths.config, &args) {
        instance::Role::Forwarded => return Ok(()),
        instance::Role::Primary(listener) => listener,
    };
    if !args.is_empty() {
        open::push(args);
    }
    #[cfg(target_os = "macos")]
    finder::install();
    let store = store::Store::new(paths.clone());

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Nullamp")
            .with_app_id("nullamp")
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([360.0, 420.0])
            .with_drag_and_drop(true)
            .with_icon(app_icon()),
        persist_window: true,
        persistence_path: Some(paths.config.join("window.ron")),
        ..Default::default()
    };
    eframe::run_native("Nullamp", options, Box::new(|cc| {
        open::set_context(cc.egui_ctx.clone());
        if let Some(listener) = listener {
            listener.spawn();
        }
        // The notification may have come first; for a running app this is the moment that matters.
        #[cfg(target_os = "macos")]
        finder::add_open_files();
        let mut app = ui::App::new(cc, store);
        app.install_native_menu();
        Ok(Box::new(app))
    }))
}

/// The window icon: the same artwork as the bundle icon, rendered by `packaging/icon.py` at 256 px.
/// Other platforms get the `--full` variant, since their docks and taskbars show icons edge to edge.
pub(crate) fn app_icon() -> egui::IconData {
    #[cfg(target_os = "macos")]
    const PNG: &[u8] = include_bytes!("../assets/icon-256.png");
    #[cfg(not(target_os = "macos"))]
    const PNG: &[u8] = include_bytes!("../assets/icon-256-full.png");
    let img = image::load_from_memory_with_format(PNG, image::ImageFormat::Png)
        .expect("bundled icon is a valid PNG")
        .into_rgba8();
    let (width, height) = img.dimensions();
    egui::IconData { rgba: img.into_raw(), width, height }
}
