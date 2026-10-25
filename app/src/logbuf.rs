//! Logging to stderr plus a small in-memory tail the About window shows.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use log::{Log, Metadata, Record};

/// Lines kept for the About window.
const KEEP: usize = 300;

static TAIL: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
static START: OnceLock<Instant> = OnceLock::new();

struct Tee(env_logger::Logger);

impl Log for Tee {
    fn enabled(&self, metadata: &Metadata) -> bool {
        self.0.enabled(metadata)
    }

    fn log(&self, record: &Record) {
        if !self.0.matches(record) {
            return;
        }
        self.0.log(record);
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
    let _ = log::set_boxed_logger(Box::new(Tee(inner)));
}

/// The most recent lines, oldest first.
pub fn tail() -> Vec<String> {
    TAIL.lock().map(|t| t.iter().cloned().collect()).unwrap_or_default()
}
