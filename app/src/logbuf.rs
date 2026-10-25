//! Logging to stderr plus a small in-memory tail the About window shows.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use log::{Log, Metadata, Record};

/// Lines kept for the About window.
const KEEP: usize = 300;

static TAIL: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
static START: OnceLock<Instant> = OnceLock::new();

/// The `env_logger` that filters and prints, the tail the About window shows, and on Android logcat.
struct Tee(env_logger::Logger, #[cfg(target_os = "android")] android_logger::AndroidLogger);

impl Log for Tee {
    fn enabled(&self, metadata: &Metadata) -> bool {
        self.0.enabled(metadata)
    }

    fn log(&self, record: &Record) {
        if !self.0.matches(record) {
            return;
        }
        self.0.log(record);
        #[cfg(target_os = "android")]
        self.1.log(record);
        let secs = START.get_or_init(Instant::now).elapsed().as_secs_f32();
        let line = format!("{secs:8.2}s {:<5} {}: {}", record.level(), record.target(), record.args());
        if let Ok(mut tail) = TAIL.lock() {
            if tail.len() == KEEP {
                tail.pop_front();
            }
            tail.push_back(line);
        }
    }

    fn flush(&self) {
        self.0.flush();
    }
}

/// Install the logger; `RUST_LOG` overrides `default_filter`.
pub fn init(default_filter: &str) {
    START.get_or_init(Instant::now);
    let inner = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(default_filter)).build();
    log::set_max_level(inner.filter());
    // The filter above already decided; logcat takes whatever reaches it.
    #[cfg(target_os = "android")]
    let tee = Tee(inner, android_logger::AndroidLogger::new(android_logger::Config::default().with_max_level(log::LevelFilter::Trace).with_tag("nullamp")));
    #[cfg(not(target_os = "android"))]
    let tee = Tee(inner);
    let _ = log::set_boxed_logger(Box::new(tee));
}

/// The most recent lines, oldest first.
pub fn tail() -> Vec<String> {
    TAIL.lock().map(|t| t.iter().cloned().collect()).unwrap_or_default()
}
