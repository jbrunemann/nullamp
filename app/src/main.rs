//! Nullamp: a retro-inspired music and DJ mix player.

// Release builds on Windows are GUI-subsystem programs so no console window opens behind the player.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod logbuf;
mod paths;
mod player;
mod store;
mod ui;
mod waveform_cache;
mod workers;

#[cfg(test)]
mod ui_snapshots;

fn main() -> eframe::Result {
    logbuf::init("warn,nullamp=info,nullamp_audio=info");
    let paths = paths::AppPaths::detect();
    log::info!("config: {}", paths.config.display());
    let store = store::Store::new(paths.clone());

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Nullamp")
            .with_app_id("nullamp")
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([640.0, 420.0])
            .with_drag_and_drop(true)
            .with_icon(app_icon()),
        persist_window: true,
        persistence_path: Some(paths.config.join("window.ron")),
        ..Default::default()
    };
    eframe::run_native("Nullamp", options, Box::new(|cc| {
        let mut app = ui::App::new(cc, store);
        app.install_native_menu();
        Ok(Box::new(app))
    }))
}

/// The window icon, drawn procedurally so no binary asset is needed at runtime.
fn app_icon() -> egui::IconData {
    let size = 64u32;
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    let c = size as f32 / 2.0;
    for y in 0..size {
        for x in 0..size {
            let (dx, dy) = (x as f32 + 0.5 - c, y as f32 + 0.5 - c);
            let r = (dx * dx + dy * dy).sqrt();
            let i = ((y * size + x) * 4) as usize;
            let px: [u8; 4] = if r > 30.0 {
                [0, 0, 0, 0]
            } else if r > 27.0 {
                [0x2E, 0x33, 0x3A, 255]
            } else if r < 7.0 {
                [0xFF, 0x8A, 0x2B, 255]
            } else if (r as i32) % 4 == 0 {
                [0x14, 0x24, 0x20, 255]
            } else {
                [0x0B, 0x10, 0x0F, 255]
            };
            rgba[i..i + 4].copy_from_slice(&px);
        }
    }
    egui::IconData { rgba, width: size, height: size }
}
