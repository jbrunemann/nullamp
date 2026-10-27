//! Keeps the player running while Android has the window stopped.
//!
//! eframe drops its window when the activity is suspended and runs no frames until it is back, so `App::logic`, which
//! turns engine events into the next track and lock screen presses into commands, stops with it. The audio keeps
//! playing from its own threads (the media service keeps the process alive), but nothing would pick the next file or
//! answer a press on the headset. `Shared` hands the app to a thread that runs the event processing at the pace of a
//! frame loop whenever no frame has run for a moment.

use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use super::App;

/// How long without a frame counts as the window being stopped.
const STALLED: Duration = Duration::from_millis(600);
/// The pace of the thread while stopped; the track-change latency of a press or a file ending.
const TICK: Duration = Duration::from_millis(200);

pub struct Shared {
    app: Arc<Mutex<App>>,
    last_frame: Arc<Mutex<Instant>>,
}

impl Shared {
    pub fn new(app: App) -> Self {
        let app = Arc::new(Mutex::new(app));
        let last_frame = Arc::new(Mutex::new(Instant::now()));
        let (weak, frame) = (Arc::downgrade(&app), last_frame.clone());
        let spawned = std::thread::Builder::new().name("nullamp-background".into()).spawn(move || tick_while_stopped(weak, frame));
        if let Err(e) = spawned {
            log::warn!("no background thread: {e}");
        }
        Shared { app, last_frame }
    }

    fn app(&self) -> std::sync::MutexGuard<'_, App> {
        self.app.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn stamp(&self) {
        if let Ok(mut at) = self.last_frame.lock() {
            *at = Instant::now();
        }
    }
}

fn tick_while_stopped(app: Weak<Mutex<App>>, last_frame: Arc<Mutex<Instant>>) {
    loop {
        std::thread::sleep(TICK);
        let Some(app) = app.upgrade() else { return };
        let stalled = last_frame.lock().map(|at| at.elapsed() > STALLED).unwrap_or(false);
        if !stalled {
            continue;
        }
        // A frame that starts now holds the lock; the next tick finds it free.
        if let Ok(mut app) = app.try_lock() {
            app.process_events();
        }
    }
}

impl eframe::App for Shared {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.stamp();
        self.app().logic(ctx, frame);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.stamp();
        self.app().ui(ui, frame);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.app().save(storage);
    }

    fn on_exit(&mut self) {
        self.app().on_exit();
    }
}
