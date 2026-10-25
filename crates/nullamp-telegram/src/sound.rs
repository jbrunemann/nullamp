//! Telling a sound file from other replies, and naming the downloaded file.

use std::path::{Path, PathBuf};

const EXTENSIONS: &[&str] = &["mp3", "flac", "wav", "ogg", "oga", "m4a", "aac", "aif", "aiff", "caf", "mka", "alac"];

/// Whether a document is a sound file: an audio MIME type, or a file name that ends like one.
pub fn is_sound(mime: Option<&str>, name: Option<&str>) -> bool {
    mime.is_some_and(|m| m.to_ascii_lowercase().starts_with("audio/")) || extension(name.unwrap_or("")).is_some_and(|e| EXTENSIONS.contains(&e.as_str()))
}

fn extension(name: &str) -> Option<String> {
    Path::new(name).extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase)
}

/// The file extension for an audio MIME type, `None` when it is not one we know.
fn mime_extension(mime: &str) -> Option<&'static str> {
    Some(match mime.to_ascii_lowercase().as_str() {
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/flac" | "audio/x-flac" => "flac",
        "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
        "audio/ogg" => "ogg",
        "audio/mp4" | "audio/x-m4a" | "audio/m4a" => "m4a",
        "audio/aac" => "aac",
        "audio/aiff" | "audio/x-aiff" => "aiff",
        _ => return None,
    })
}

/// A file name that is safe to create in a folder: the document's own name when it has one, else
/// "Performer - Title", with an extension from the name or the MIME type.
pub fn file_name(name: Option<&str>, performer: Option<&str>, title: Option<&str>, mime: Option<&str>) -> String {
    let named = name.map(clean).filter(|n| !n.is_empty());
    let base = named.unwrap_or_else(|| match (performer.map(clean).filter(|p| !p.is_empty()), title.map(clean).filter(|t| !t.is_empty())) {
        (Some(p), Some(t)) => format!("{p} - {t}"),
        (None, Some(t)) => t,
        (Some(p), None) => p,
        (None, None) => "Telegram track".to_owned(),
    });
    if extension(&base).is_some_and(|e| EXTENSIONS.contains(&e.as_str())) {
        return base;
    }
    format!("{base}.{}", mime.and_then(mime_extension).unwrap_or("mp3"))
}

/// What a sound file is, for a list: "MP3, 4.2 MB, 3:45", leaving out what is not known.
pub fn describe_file(name: &str, size: Option<usize>, seconds: Option<f64>) -> String {
    let mut parts: Vec<String> = extension(name).map(|e| e.to_ascii_uppercase()).into_iter().collect();
    if let Some(size) = size {
        parts.push(if size >= 1_000_000 { format!("{:.1} MB", size as f64 / 1_000_000.0) } else { format!("{} KB", size.div_ceil(1000)) });
    }
    if let Some(s) = seconds.filter(|s| *s >= 1.0) {
        parts.push(format!("{}:{:02}", s as u64 / 60, s as u64 % 60));
    }
    parts.join(", ")
}

/// The last path component of `name` with characters a file system dislikes replaced, and no
/// leading dots (a hidden file), cut to a length that leaves room for ` (2)` and the extension.
fn clean(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or("");
    let replaced: String = last.chars().map(|c| if c.is_control() || c == ':' { '_' } else { c }).collect();
    let trimmed = replaced.trim().trim_start_matches('.');
    let mut end = trimmed.len().min(150);
    while !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    trimmed[..end].trim_end().to_owned()
}

/// `name` in `dir`, or `name (2)`, `name (3)`... when that exists, so a download never replaces a file.
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    if !path.exists() {
        return path;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, format!(".{e}")),
        _ => (name, String::new()),
    };
    (2..).map(|n| dir.join(format!("{stem} ({n}){ext}"))).find(|p| !p.exists()).expect("an unused name")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sound_by_mime_or_extension() {
        assert!(is_sound(Some("audio/mpeg"), None));
        assert!(is_sound(None, Some("Track.FLAC")));
        assert!(is_sound(Some("application/octet-stream"), Some("a.mp3")));
        assert!(!is_sound(Some("application/pdf"), Some("a.pdf")));
        assert!(!is_sound(Some("image/jpeg"), None));
        assert!(!is_sound(None, None));
    }

    #[test]
    fn names_are_safe_and_have_an_extension() {
        assert_eq!(file_name(Some("../../etc/pass:wd.mp3"), None, None, None), "pass_wd.mp3");
        assert_eq!(file_name(Some(".hidden.mp3"), None, None, None), "hidden.mp3");
        assert_eq!(file_name(None, Some("Donnie"), Some("Troopa"), Some("audio/mpeg")), "Donnie - Troopa.mp3");
        assert_eq!(file_name(None, None, Some("Troopa"), Some("audio/flac")), "Troopa.flac");
        assert_eq!(file_name(Some("Troopa"), None, None, Some("audio/mp4")), "Troopa.m4a");
        assert_eq!(file_name(None, None, None, None), "Telegram track.mp3");
    }

    #[test]
    fn files_are_described_for_a_list() {
        assert_eq!(describe_file("A - B.mp3", Some(4_235_000), Some(225.4)), "MP3, 4.2 MB, 3:45");
        assert_eq!(describe_file("x.flac", Some(800), None), "FLAC, 1 KB");
        assert_eq!(describe_file("noext", None, Some(0.0)), "");
    }

    #[test]
    fn long_names_are_cut_on_a_character() {
        let name = file_name(Some(&"é".repeat(200)), None, None, None);
        assert!(name.ends_with(".mp3") && name.len() <= 160, "{}", name.len());
    }

    #[test]
    fn existing_files_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(unique_path(dir.path(), "a.mp3"), dir.path().join("a.mp3"));
        std::fs::write(dir.path().join("a.mp3"), b"x").unwrap();
        assert_eq!(unique_path(dir.path(), "a.mp3"), dir.path().join("a (2).mp3"));
        std::fs::write(dir.path().join("a (2).mp3"), b"x").unwrap();
        assert_eq!(unique_path(dir.path(), "a.mp3"), dir.path().join("a (3).mp3"));
    }
}
