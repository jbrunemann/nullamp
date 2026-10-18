use std::path::Path;

use serde::{Deserialize, Serialize};

/// The display-level description of an audio file, as shown in lists.
///
/// This is a cache of what the tag reader found; the authoritative data lives in
/// the file and is handled by the metadata crate.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TrackInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    /// Duration in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
}

impl TrackInfo {
    /// Title to display, falling back to the file stem.
    pub fn display_title(&self, path: &Path) -> String {
        match self.title.as_deref().map(str::trim) {
            Some(t) if !t.is_empty() => t.to_owned(),
            _ => file_stem(path),
        }
    }

    /// `Artist - Title`, or just the title.
    pub fn display_line(&self, path: &Path) -> String {
        let title = self.display_title(path);
        match self.artist.as_deref().map(str::trim) {
            Some(a) if !a.is_empty() && self.title.is_some() => format!("{a} - {title}"),
            _ => title,
        }
    }
}

pub fn file_stem(path: &Path) -> String {
    path.file_stem()
        .or_else(|| path.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn falls_back_to_file_stem() {
        let info = TrackInfo::default();
        assert_eq!(info.display_title(Path::new("/m/Artist - Song.mp3")), "Artist - Song");
        assert_eq!(info.display_line(Path::new("/m/Artist - Song.mp3")), "Artist - Song");
        let tagged = TrackInfo {
            title: Some("Song".into()),
            artist: Some("Artist".into()),
            ..Default::default()
        };
        assert_eq!(tagged.display_line(Path::new("/x.mp3")), "Artist - Song");
    }
}
