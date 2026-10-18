//! M3U / M3U8 playlist import and export.
//!
//! Reading accepts plain and extended M3U, absolute and relative paths, Windows-style
//! separators and `file://` URLs. Remote URLs are reported as skipped rather than
//! silently dropped. Writing always produces UTF-8 extended M3U.

use std::path::{Component, Path, PathBuf};

use percent_encoding::percent_decode_str;

use crate::text::decode_lossy;
use crate::track::TrackInfo;

#[derive(Clone, Debug, PartialEq)]
pub struct M3uItem {
    pub path: PathBuf,
    /// Information from a preceding `#EXTINF` line, if any.
    pub info: TrackInfo,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Skipped {
    pub line: usize,
    pub text: String,
    pub reason: &'static str,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct M3uParse {
    pub items: Vec<M3uItem>,
    pub skipped: Vec<Skipped>,
    /// `#PLAYLIST:` title, if present.
    pub title: Option<String>,
}

/// Parse playlist bytes. Relative paths are resolved against `base_dir`
/// (normally the directory containing the playlist file).
pub fn parse(bytes: &[u8], base_dir: &Path) -> M3uParse {
    let text = decode_lossy(bytes);
    let mut out = M3uParse::default();
    let mut pending: Option<TrackInfo> = None;

    for (idx, raw) in text.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            pending = Some(parse_extinf(rest));
            continue;
        }
        if let Some(title) = line.strip_prefix("#PLAYLIST:") {
            out.title = Some(title.trim().to_owned()).filter(|t| !t.is_empty());
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        let info = pending.take().unwrap_or_default();
        match resolve_location(line, base_dir) {
            Ok(path) => out.items.push(M3uItem { path, info }),
            Err(reason) => out.skipped.push(Skipped { line: line_no, text: line.to_owned(), reason }),
        }
    }
    out
}

fn parse_extinf(rest: &str) -> TrackInfo {
    // `#EXTINF:<seconds>[ attributes],<display>`. Attributes (key="value") may contain
    // commas inside quotes, so find the first comma outside quotes.
    let mut in_quotes = false;
    let mut split_at = None;
    for (i, c) in rest.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                split_at = Some(i);
                break;
            }
            _ => {}
        }
    }
    let (head, display) = match split_at {
        Some(i) => (&rest[..i], rest[i + 1..].trim()),
        None => (rest, ""),
    };
    let duration = head
        .split_whitespace()
        .next()
        .and_then(|d| d.parse::<f64>().ok())
        .filter(|d| d.is_finite() && *d > 0.0);
    let mut info = TrackInfo { duration, ..Default::default() };
    if !display.is_empty() {
        match display.split_once(" - ") {
            Some((artist, title)) if !artist.trim().is_empty() && !title.trim().is_empty() => {
                info.artist = Some(artist.trim().to_owned());
                info.title = Some(title.trim().to_owned());
            }
            _ => info.title = Some(display.to_owned()),
        }
    }
    info
}

fn resolve_location(location: &str, base_dir: &Path) -> Result<PathBuf, &'static str> {
    if let Some(rest) = location.strip_prefix("file://") {
        // file:///abs/path or file://localhost/abs/path
        let rest = rest.strip_prefix("localhost").unwrap_or(rest);
        let decoded = percent_decode_str(rest).decode_utf8().map_err(|_| "invalid file URL encoding")?;
        let decoded = decoded.into_owned();
        // file:///C:/Music on Windows.
        #[cfg(windows)]
        let decoded = decoded.strip_prefix('/').filter(|s| s.as_bytes().get(1) == Some(&b':')).map(str::to_owned).unwrap_or(decoded);
        return Ok(normalize(PathBuf::from(decoded)));
    }
    if is_remote_url(location) {
        return Err("remote streams are not supported");
    }
    let native = to_native_separators(location);
    let path = PathBuf::from(&native);
    if path.is_absolute() || looks_like_windows_absolute(location) {
        Ok(normalize(path))
    } else {
        Ok(normalize(base_dir.join(path)))
    }
}

fn is_remote_url(s: &str) -> bool {
    match s.split_once("://") {
        Some((scheme, _)) => !scheme.is_empty() && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c)),
        None => false,
    }
}

fn looks_like_windows_absolute(s: &str) -> bool {
    let b = s.as_bytes();
    (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')) || s.starts_with("\\\\")
}

fn to_native_separators(s: &str) -> String {
    if cfg!(windows) { s.replace('/', "\\") } else { s.replace('\\', "/") }
}

/// Remove `.` and resolve `..` lexically (without touching the filesystem).
fn normalize(path: PathBuf) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

pub struct WriteOptions {
    /// Write paths relative to the playlist's directory when possible.
    pub relative_to: Option<PathBuf>,
}

/// Produce UTF-8 extended M3U text.
pub fn write<'a>(items: impl IntoIterator<Item = (&'a Path, &'a TrackInfo)>, opts: &WriteOptions) -> String {
    let mut out = String::from("#EXTM3U\n");
    for (path, info) in items {
        let secs = info.duration.filter(|d| d.is_finite() && *d > 0.0).map(|d| d.round() as i64).unwrap_or(-1);
        let display = match (&info.artist, &info.title) {
            (Some(a), Some(t)) => format!("{a} - {t}"),
            (None, Some(t)) => t.clone(),
            _ => crate::track::file_stem(path),
        };
        out.push_str(&format!("#EXTINF:{secs},{}\n", display.replace(['\r', '\n'], " ")));
        let written = opts
            .relative_to
            .as_deref()
            .and_then(|base| relative_path(path, base))
            .unwrap_or_else(|| path.to_path_buf());
        out.push_str(&written.to_string_lossy());
        out.push('\n');
    }
    out
}

/// Path of `path` relative to `base`, if `path` is inside `base`'s tree or a sibling tree
/// on the same root. Returns `None` when no sensible relative form exists.
fn relative_path(path: &Path, base: &Path) -> Option<PathBuf> {
    if !path.is_absolute() || !base.is_absolute() {
        return None;
    }
    let p: Vec<_> = path.components().collect();
    let b: Vec<_> = base.components().collect();
    // Different roots / drive letters: no relative form.
    if p.first() != b.first() {
        return None;
    }
    let common = p.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut rel = PathBuf::new();
    for _ in common..b.len() {
        rel.push("..");
    }
    for c in &p[common..] {
        rel.push(c.as_os_str());
    }
    Some(rel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_extended_m3u() {
        let text = "#EXTM3U\n#PLAYLIST:Friday\n#EXTINF:215,Daft Punk - One More Time\nmusic/one.mp3\n\n# a comment\n/abs/two.flac\n#EXTINF:-1,Just A Title\nthree.mp3\n";
        let p = parse(text.as_bytes(), Path::new("/lists"));
        assert_eq!(p.title.as_deref(), Some("Friday"));
        assert_eq!(p.items.len(), 3);
        assert_eq!(p.items[0].path, PathBuf::from("/lists/music/one.mp3"));
        assert_eq!(p.items[0].info.artist.as_deref(), Some("Daft Punk"));
        assert_eq!(p.items[0].info.title.as_deref(), Some("One More Time"));
        assert_eq!(p.items[0].info.duration, Some(215.0));
        assert_eq!(p.items[1].path, PathBuf::from("/abs/two.flac"));
        assert_eq!(p.items[1].info, TrackInfo::default());
        assert_eq!(p.items[2].info.title.as_deref(), Some("Just A Title"));
        assert_eq!(p.items[2].info.duration, None);
    }

    #[test]
    fn extinf_attributes_with_commas() {
        let info = parse_extinf(r#"120 tvg-name="a, b" logo="x",Artist - Title"#);
        assert_eq!(info.duration, Some(120.0));
        assert_eq!(info.artist.as_deref(), Some("Artist"));
    }

    #[test]
    fn plain_m3u_crlf_and_relative_parents() {
        let text = "../other/a.mp3\r\n./b.mp3\r\n";
        let p = parse(text.as_bytes(), Path::new("/music/lists"));
        assert_eq!(p.items[0].path, PathBuf::from("/music/other/a.mp3"));
        assert_eq!(p.items[1].path, PathBuf::from("/music/lists/b.mp3"));
    }

    #[cfg(not(windows))]
    #[test]
    fn windows_relative_separators() {
        let p = parse(b"sub\\dir\\c.mp3\n", Path::new("/m"));
        assert_eq!(p.items[0].path, PathBuf::from("/m/sub/dir/c.mp3"));
    }

    #[cfg(not(windows))]
    #[test]
    fn file_urls_are_decoded() {
        let p = parse(b"file:///Users/me/My%20Music/caf%C3%A9.mp3\nfile://localhost/tmp/x.mp3\n", Path::new("/"));
        assert_eq!(p.items[0].path, PathBuf::from("/Users/me/My Music/café.mp3"));
        assert_eq!(p.items[1].path, PathBuf::from("/tmp/x.mp3"));
    }

    #[test]
    fn remote_urls_are_skipped_and_reported() {
        let p = parse(b"http://radio.example/stream\nlocal.mp3\n", Path::new("/m"));
        assert_eq!(p.items.len(), 1);
        assert_eq!(p.skipped.len(), 1);
        assert_eq!(p.skipped[0].line, 1);
    }

    #[test]
    fn legacy_encoding_fallback() {
        let p = parse(b"#EXTINF:10,Bj\xF6rk - J\xF3ga\nbjork.mp3\n", Path::new("/m"));
        assert_eq!(p.items[0].info.artist.as_deref(), Some("Björk"));
        assert_eq!(p.items[0].info.title.as_deref(), Some("Jóga"));
    }

    #[cfg(not(windows))]
    #[test]
    fn write_then_read_round_trip() {
        let a = TrackInfo { title: Some("Tïtle".into()), artist: Some("Ärtist".into()), duration: Some(61.4), ..Default::default() };
        let b = TrackInfo::default();
        let items = [(Path::new("/music/x/a.mp3"), &a), (Path::new("/elsewhere/b.mp3"), &b)];
        let text = write(items, &WriteOptions { relative_to: Some("/music/lists".into()) });
        assert!(text.starts_with("#EXTM3U\n#EXTINF:61,Ärtist - Tïtle\n../x/a.mp3\n"));
        assert!(text.contains("#EXTINF:-1,b\n../../elsewhere/b.mp3\n"));
        let back = parse(text.as_bytes(), Path::new("/music/lists"));
        assert_eq!(back.items[0].path, PathBuf::from("/music/x/a.mp3"));
        assert_eq!(back.items[0].info.title.as_deref(), Some("Tïtle"));
        assert_eq!(back.items[1].path, PathBuf::from("/elsewhere/b.mp3"));
    }
}
