//! Nullamp: a retro-inspired music and DJ mix player.
//!
//! The desktop program is `main.rs`; the Android app is the `android` crate, which hands its activity to [`run_android`].

mod cache;
mod entry_art;
// Android logs to logcat instead; the About window still shows the (empty) tail.
#[cfg_attr(target_os = "android", allow(dead_code))]
mod logbuf;
mod mixxx;
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

/// The desktop program.
#[cfg(not(target_os = "android"))]
pub fn run() -> eframe::Result {
    // lofty warns on every UTF-8 frame it rewrites as UTF-16 when saving ID3v2.3; the output is correct.
    // symphonia warns about a missing bit reservoir and resync junk after coarse MP3 seeks; a frame is dropped, nothing audible.
    logbuf::init("warn,lofty::util::text=error,lofty::mpeg::properties=error,symphonia_bundle_mp3::layer3=error,symphonia_bundle_mp3::demuxer=error,nullamp=info,nullamp_audio=info");
    let paths = paths::AppPaths::detect();
    log::info!("config: {}", paths.config.display());
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

/// The activity, kept for the system bar insets.
#[cfg(target_os = "android")]
static ANDROID_APP: std::sync::OnceLock<android_activity::AndroidApp> = std::sync::OnceLock::new();

/// How far the status bar and the gesture bar cover the window, in points: (top, bottom). Always zero off Android.
/// Asked of the window over JNI twice a second: `AndroidApp::content_rect` reports a top that includes an
/// action bar this app does not have.
#[cfg(target_os = "android")]
pub(crate) fn system_bar_insets(ppp: f32) -> (f32, f32) {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    static LAST: Mutex<Option<(Instant, (i32, i32))>> = Mutex::new(None);
    let Some(app) = ANDROID_APP.get() else { return (0.0, 0.0) };
    let Ok(mut last) = LAST.lock() else { return (0.0, 0.0) };
    let fresh = (*last).filter(|(at, _)| at.elapsed() < Duration::from_millis(500));
    let (_, (top, bottom)) = fresh.unwrap_or_else(|| {
        let px = window_insets(app).unwrap_or_else(|e| {
            log::warn!("window insets: {e}");
            (0, 0)
        });
        let now = (Instant::now(), px);
        *last = Some(now);
        now
    });
    (top as f32 / ppp, bottom as f32 / ppp)
}

/// The system window insets (top, bottom) in pixels: `activity.getWindow().getDecorView().getRootWindowInsets()`.
#[cfg(target_os = "android")]
fn window_insets(app: &android_activity::AndroidApp) -> jni::errors::Result<(i32, i32)> {
    use jni::objects::JObject;
    use jni::{jni_sig, jni_str};
    // SAFETY: the pointer is the VM the activity runs in, valid for the life of the process.
    let vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr().cast()) };
    vm.attach_current_thread(|env| {
        // SAFETY: the activity is a global reference android-activity owns, so it is never deleted here.
        let activity = std::mem::ManuallyDrop::new(unsafe { JObject::from_raw(env, app.activity_as_ptr() as jni::sys::jobject) });
        let window = env.call_method(&*activity, jni_str!("getWindow"), jni_sig!("()Landroid/view/Window;"), &[])?.l()?;
        let decor = env.call_method(&window, jni_str!("getDecorView"), jni_sig!("()Landroid/view/View;"), &[])?.l()?;
        let insets = env.call_method(&decor, jni_str!("getRootWindowInsets"), jni_sig!("()Landroid/view/WindowInsets;"), &[])?.l()?;
        let top = env.call_method(&insets, jni_str!("getSystemWindowInsetTop"), jni_sig!("()I"), &[])?.i()?;
        let bottom = env.call_method(&insets, jni_str!("getSystemWindowInsetBottom"), jni_sig!("()I"), &[])?.i()?;
        Ok((top, bottom))
    })
}

#[cfg(not(target_os = "android"))]
pub(crate) fn system_bar_insets(_ppp: f32) -> (f32, f32) {
    (0.0, 0.0)
}

/// The Android app: the `android` crate hands its activity in. Everything lives in the app's private folder.
/// There is no file dialog yet, so "Add folder" adds the music folders that `rfd` below names instead.
#[cfg(target_os = "android")]
pub fn run_android(android_app: android_activity::AndroidApp) {
    android_logger::init_once(android_logger::Config::default().with_max_level(log::LevelFilter::Info).with_tag("nullamp"));
    // SAFETY: nothing else is running yet, so nothing reads the environment concurrently.
    unsafe {
        if let Some(home) = android_app.internal_data_path() {
            std::env::set_var("NULLAMP_HOME", home);
        }
        if std::env::var_os("WGPU_BACKEND").is_none() {
            std::env::set_var("WGPU_BACKEND", "vulkan");
        }
        if let Some(files) = android_app.external_data_path() {
            // Made by the app, since a folder made by `adb` is not readable by it.
            let music = files.join("Music");
            let _ = std::fs::create_dir_all(&music);
            std::env::set_var("NULLAMP_MUSIC", music);
        }
    }
    let _ = ANDROID_APP.set(android_app.clone());
    let paths = paths::AppPaths::detect();
    log::info!("config: {}", paths.config.display());
    let store = store::Store::new(paths.clone());
    let options = eframe::NativeOptions {
        persist_window: false,
        persistence_path: Some(paths.config.join("window.ron")),
        android_app: Some(android_app),
        ..Default::default()
    };
    if let Err(e) = eframe::run_native("Nullamp", options, Box::new(|cc| Ok(Box::new(ui::App::new(cc, store))))) {
        log::error!("nullamp stopped: {e}");
    }
}
