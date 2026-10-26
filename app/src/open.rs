//! Files that reach the running app from outside: command-line paths, a second launch and Finder.
//!
//! Every source calls `push`; the UI drains `take` once per frame. A static queue is used because Finder
//! can deliver files before the window exists.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

#[derive(Default)]
pub struct Request {
    pub paths: Vec<PathBuf>,
    /// The window should come to the front, even when `paths` is empty (a second launch without files).
    pub raise: bool,
}

static INBOX: Mutex<Request> = Mutex::new(Request { paths: Vec::new(), raise: false });
static WAKE: OnceLock<egui::Context> = OnceLock::new();

/// Let `push` wake the UI, which sleeps while nothing happens.
pub fn set_context(ctx: egui::Context) {
    let _ = WAKE.set(ctx);
}

pub fn push(paths: Vec<PathBuf>) {
    if let Ok(mut inbox) = INBOX.lock() {
        inbox.paths.extend(paths);
        inbox.raise = true;
    }
    if let Some(ctx) = WAKE.get() {
        ctx.request_repaint();
    }
}

pub fn take() -> Request {
    INBOX.lock().map(|mut inbox| std::mem::take(&mut *inbox)).unwrap_or_default()
}

/// Existing files named on the command line, made absolute so another process can use them. Options and
/// the `-psn_...` argument old macOS launchers add are skipped.
pub fn args_paths(args: impl IntoIterator<Item = std::ffi::OsString>) -> Vec<PathBuf> {
    let cwd = std::env::current_dir().unwrap_or_default();
    args.into_iter()
        .filter(|a| !a.to_string_lossy().starts_with('-'))
        .map(|a| cwd.join(a))
        .filter(|p| p.exists())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_keep_existing_paths_and_skip_options() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.mp3");
        std::fs::write(&file, b"x").unwrap();
        let args = ["-psn_0_1234".into(), file.clone().into_os_string(), dir.path().join("gone.mp3").into_os_string()];
        assert_eq!(args_paths(args), vec![file]);
    }
}
