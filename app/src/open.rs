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

/// The path of a `file://` URI, which is how MPRIS clients and some Linux file managers name a file.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // `file://localhost/home/...` names the same file as `file:///home/...`.
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let mut bytes = Vec::with_capacity(rest.len());
    let raw = rest.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        match raw[i] {
            b'%' if i + 2 < raw.len() && raw[i + 1].is_ascii_hexdigit() && raw[i + 2].is_ascii_hexdigit() => {
                bytes.push(u8::from_str_radix(std::str::from_utf8(&raw[i + 1..i + 3]).ok()?, 16).ok()?);
                i += 3;
            }
            b => {
                bytes.push(b);
                i += 1;
            }
        }
    }
    if !bytes.starts_with(b"/") {
        return None;
    }
    #[cfg(unix)]
    return Some(PathBuf::from(<std::ffi::OsString as std::os::unix::ffi::OsStringExt>::from_vec(bytes)));
    #[cfg(not(unix))]
    {
        // `/C:/Music/a.mp3` is `C:\Music\a.mp3` on Windows.
        let text = String::from_utf8(bytes).ok()?;
        let text = text.strip_prefix('/').filter(|t| t.as_bytes().get(1) == Some(&b':')).unwrap_or(&text);
        Some(PathBuf::from(text))
    }
}

/// Existing files named on the command line (plain paths or `file://` URIs), made absolute so another process
/// can use them. Options and the `-psn_...` argument old macOS launchers add are skipped.
pub fn args_paths(args: impl IntoIterator<Item = std::ffi::OsString>) -> Vec<PathBuf> {
    let cwd = std::env::current_dir().unwrap_or_default();
    args.into_iter()
        .filter(|a| !a.to_string_lossy().starts_with('-'))
        .map(|a| a.to_str().and_then(uri_to_path).unwrap_or_else(|| cwd.join(a)))
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

    #[test]
    fn file_uris_become_paths() {
        assert_eq!(uri_to_path("file:///home/jan/My%20Music/%C3%BC.mp3"), Some(PathBuf::from("/home/jan/My Music/ü.mp3")));
        assert_eq!(uri_to_path("file://localhost/tmp/a.mp3"), Some(PathBuf::from("/tmp/a.mp3")));
        assert_eq!(uri_to_path("file:///100%/a.mp3"), Some(PathBuf::from("/100%/a.mp3")));
        assert_eq!(uri_to_path("http://example.com/a.mp3"), None);
        assert_eq!(uri_to_path("file://host/a.mp3"), None);
    }

    #[test]
    fn args_accept_file_uris() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a b.mp3");
        std::fs::write(&file, b"x").unwrap();
        let uri = format!("file://{}", file.to_str().unwrap().replace(' ', "%20"));
        assert_eq!(args_paths([uri.into()]), vec![file]);
    }
}
