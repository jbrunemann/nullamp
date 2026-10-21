use std::path::Path;

use serde::{Deserialize, Deserializer, Serialize};

use crate::key::Key;

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
    /// The year of the date tag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<u16>,
    /// Duration in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    /// Audio bitrate in kbit/s, as the file's properties report it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bitrate: Option<u32>,
    /// Beats per minute, from the file's tags or, when `bpm_detected`, found by listening.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bpm: Option<f32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bpm_detected: bool,
    /// The tags say `-1`: no BPM is shown for the track and none is detected.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bpm_off: bool,
    /// The key from the file's tags, whichever notation they used.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "key_text")]
    pub key: Option<Key>,
}

/// Saved as the Camelot code; a value that no longer parses is dropped instead of failing the playlist.
mod key_text {
    use super::*;

    pub fn serialize<S: serde::Serializer>(key: &Option<Key>, s: S) -> Result<S::Ok, S::Error> {
        key.map(Key::camelot).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Key>, D::Error> {
        Ok(Option::<String>::deserialize(d)?.and_then(|s| Key::parse(&s)))
    }
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

    #[test]
    fn key_is_saved_as_a_camelot_code_and_survives_garbage() {
        let info = TrackInfo { key: Key::parse("Am"), ..Default::default() };
        let json = serde_json::to_string(&info).unwrap();
        assert_eq!(json, r#"{"key":"8A"}"#);
        assert_eq!(serde_json::from_str::<TrackInfo>(&json).unwrap(), info);
        assert_eq!(serde_json::to_string(&TrackInfo::default()).unwrap(), "{}");
        assert_eq!(serde_json::from_str::<TrackInfo>(r#"{"key":"nonsense"}"#).unwrap().key, None);
        assert_eq!(serde_json::from_str::<TrackInfo>(r#"{"key":null}"#).unwrap().key, None);
    }
}
