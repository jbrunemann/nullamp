//! Persistence: settings, named playlists and saved setlists, as JSON files.
//!
//! Every write goes to a temporary file that is then renamed into place, so a crash
//! never leaves a truncated file. Unreadable files are moved aside (`.broken`) and
//! reported instead of being silently overwritten.

use std::collections::HashSet;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use nullamp_core::{Playlist, RepeatMode, Setlist};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::paths::AppPaths;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub volume: f32,
    pub muted: bool,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    /// Resume the last track on start. Off by default: starting the app should not
    /// make noise unless asked to.
    pub resume_playback: bool,
    pub resume: Option<ResumePoint>,
    /// Playlist file stems in display order.
    pub playlist_order: Vec<String>,
    pub active_playlist: Option<String>,
    pub online_lookups: bool,
    pub backup_before_tag_writes: bool,
    pub backup_budget_mb: u64,
    pub side_panel_open: bool,
    pub side_panel_tab: String,
    pub last_dir: Option<PathBuf>,
    /// LCD visualiser: "spectrum", "scope" or "off".
    pub visualizer: String,
    /// The playlist table. Hidden, the window shrinks to just the player.
    pub playlist_open: bool,
    /// "dark", "light" or "system" (follow the OS).
    pub appearance: String,
    /// Playlist columns shown, left to right (ids from `playlist_view::Col`). Empty means all, in the default order.
    pub playlist_columns: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            volume: 0.8,
            muted: false,
            shuffle: false,
            repeat: RepeatMode::Off,
            resume_playback: false,
            resume: None,
            playlist_order: Vec::new(),
            active_playlist: None,
            online_lookups: true,
            backup_before_tag_writes: true,
            backup_budget_mb: 2048,
            side_panel_open: true,
            side_panel_tab: "setlist".into(),
            last_dir: None,
            visualizer: "spectrum".into(),
            playlist_open: true,
            appearance: "dark".into(),
            playlist_columns: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResumePoint {
    pub playlist: String,
    pub index: usize,
    pub path: PathBuf,
    pub position: f64,
}

/// A playlist and the file it is stored in.
#[derive(Clone, Debug)]
pub struct PlaylistDoc {
    /// File stem; stable across renames.
    pub key: String,
    pub playlist: Playlist,
}

/// A setlist saved for an audio file, keyed by content fingerprint so it follows
/// the file when it is moved or renamed.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedSetlist {
    pub fingerprint: String,
    pub last_path: PathBuf,
    pub setlist: Setlist,
}

pub struct Store {
    pub paths: AppPaths,
    /// Problems found while loading, to show once in the UI.
    pub load_warnings: Vec<String>,
}

impl Store {
    pub fn new(paths: AppPaths) -> Self {
        Store { paths, load_warnings: Vec::new() }
    }

    pub fn load_settings(&mut self) -> Settings {
        self.read_json(&self.paths.config.join("settings.json")).unwrap_or_default()
    }

    pub fn save_settings(&self, s: &Settings) -> std::io::Result<()> {
        write_json(&self.paths.config.join("settings.json"), s)
    }

    /// Load all playlists in the saved order (unknown files appended by name).
    pub fn load_playlists(&mut self, order: &[String]) -> Vec<PlaylistDoc> {
        let dir = self.paths.playlists();
        let mut keys: Vec<String> = fs::read_dir(&dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter_map(|e| {
                        let p = e.path();
                        (p.extension().is_some_and(|x| x == "json")).then(|| p.file_stem()?.to_str().map(str::to_owned))?
                    })
                    .collect()
            })
            .unwrap_or_default();
        keys.sort();
        let mut ordered: Vec<String> = order.iter().filter(|k| keys.contains(k)).cloned().collect();
        ordered.extend(keys.into_iter().filter(|k| !order.contains(k)));
        ordered
            .into_iter()
            .filter_map(|key| {
                let playlist: Playlist = self.read_json(&dir.join(format!("{key}.json")))?;
                Some(PlaylistDoc { key, playlist })
            })
            .collect()
    }

    pub fn save_playlist(&self, doc: &PlaylistDoc) -> std::io::Result<()> {
        write_json(&self.paths.playlists().join(format!("{}.json", doc.key)), &doc.playlist)
    }

    pub fn delete_playlist(&self, key: &str) -> std::io::Result<()> {
        match fs::remove_file(self.paths.playlists().join(format!("{key}.json"))) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// A file stem for a new playlist, derived from its name and unique.
    pub fn new_playlist_key(&self, name: &str, taken: &HashSet<String>) -> String {
        let base: String = name
            .chars()
            .map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
            .collect::<String>()
            .split('-')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("-");
        let base = if base.is_empty() { "playlist".to_owned() } else { base.chars().take(40).collect() };
        let exists = |k: &str| taken.contains(k) || self.paths.playlists().join(format!("{k}.json")).exists();
        if !exists(&base) {
            return base;
        }
        (2..).map(|n| format!("{base}-{n}")).find(|k| !exists(k)).unwrap()
    }

    fn read_json<T: DeserializeOwned>(&mut self, path: &Path) -> Option<T> {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                self.load_warnings.push(format!("Cannot read {}: {e}", path.display()));
                return None;
            }
        };
        match serde_json::from_slice(&bytes) {
            Ok(v) => Some(v),
            Err(e) => {
                let aside = path.with_extension("json.broken");
                let _ = fs::rename(path, &aside);
                self.load_warnings.push(format!("{} was unreadable ({e}); kept as {}", path.display(), aside.display()));
                None
            }
        }
    }
}

/// Save (or with `None`, delete) the setlist for a fingerprint. Safe to call from
/// worker threads.
pub fn save_setlist(paths: &AppPaths, fingerprint: &str, saved: Option<&SavedSetlist>) -> std::io::Result<()> {
    if !valid_fingerprint(fingerprint) {
        return Err(std::io::Error::other("invalid fingerprint"));
    }
    let path = paths.setlists().join(format!("{fingerprint}.json"));
    match saved {
        Some(s) => write_json(&path, s),
        None => match fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        },
    }
}

/// Find the saved setlist for an audio file: by fingerprint, else by its last known
/// path. Safe to call from worker threads.
pub fn find_setlist(paths: &AppPaths, fingerprint: &str, path: &Path) -> Option<SavedSetlist> {
    let dir = paths.setlists();
    if valid_fingerprint(fingerprint)
        && let Ok(bytes) = fs::read(dir.join(format!("{fingerprint}.json")))
        && let Ok(saved) = serde_json::from_slice::<SavedSetlist>(&bytes)
    {
        return Some(saved);
    }
    fs::read_dir(&dir).ok()?.filter_map(|e| e.ok()).find_map(|e| {
        let saved: SavedSetlist = serde_json::from_slice(&fs::read(e.path()).ok()?).ok()?;
        (saved.last_path == path).then_some(saved)
    })
}

fn valid_fingerprint(f: &str) -> bool {
    !f.is_empty() && f.len() <= 64 && f.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let bytes = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

/// Content fingerprint of an audio file, stable when the file is moved, renamed or
/// retagged: hashes of 64 KiB just after a leading ID3v2 tag and 64 KiB just before
/// a trailing ID3v1 tag. (Formats that keep tags at the start, such as FLAC and
/// Ogg, change the head hash when retagged; the tail still matches, and setlists are
/// also found by path as a fallback.)
pub fn fingerprint(path: &Path) -> std::io::Result<String> {
    const CHUNK: u64 = 64 * 1024;
    let mut f = fs::File::open(path)?;
    let len = f.metadata()?.len();
    let mut header = [0u8; 10];
    let mut head_start = 0u64;
    if len >= 10 {
        f.read_exact(&mut header)?;
        if &header[..3] == b"ID3" {
            let size = header[6..10].iter().fold(0u64, |acc, b| (acc << 7) | u64::from(b & 0x7F));
            let footer = if header[5] & 0x10 != 0 { 10 } else { 0 };
            head_start = (10 + size + footer).min(len);
        }
    }
    let mut tail_end = len;
    if len >= 128 {
        let mut tag = [0u8; 3];
        f.seek(SeekFrom::Start(len - 128))?;
        f.read_exact(&mut tag)?;
        if &tag == b"TAG" {
            tail_end = len - 128;
        }
    }
    let mut window = |start: u64, end: u64| -> std::io::Result<u64> {
        let start = start.min(end);
        let mut buf = vec![0u8; (end - start) as usize];
        f.seek(SeekFrom::Start(start))?;
        f.read_exact(&mut buf)?;
        Ok(nullamp_online::fnv1a(&buf))
    };
    let head = window(head_start, (head_start + CHUNK).min(tail_end))?;
    let tail = window(tail_end.saturating_sub(CHUNK).max(head_start), tail_end)?;
    Ok(format!("{head:016x}{tail:016x}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nullamp_core::setlist::{Provenance, SetlistEntry};

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let paths = AppPaths { config: base.join("c"), data: base.join("d"), cache: base.join("k") };
        (dir, Store::new(paths))
    }

    #[test]
    fn playlists_persist_in_order() {
        let (_d, mut s) = store();
        let mut a = Playlist::new("Friday Night");
        a.add_paths(["/m/1.mp3".into(), "/m/2.mp3".into()]);
        let b = Playlist::new("Ambient");
        let ka = s.new_playlist_key(&a.name, &HashSet::new());
        assert_eq!(ka, "friday-night");
        s.save_playlist(&PlaylistDoc { key: ka.clone(), playlist: a }).unwrap();
        let kb = s.new_playlist_key("Ambient", &HashSet::new());
        s.save_playlist(&PlaylistDoc { key: kb.clone(), playlist: b }).unwrap();
        assert_eq!(s.new_playlist_key("Friday  night!", &HashSet::new()), "friday-night-2");

        let loaded = s.load_playlists(&[ka.clone(), kb.clone()]);
        assert_eq!(loaded.iter().map(|d| d.key.as_str()).collect::<Vec<_>>(), [ka.as_str(), kb.as_str()]);
        let names: Vec<_> = loaded[0].playlist.entries().iter().map(|e| e.path.clone()).collect();
        assert_eq!(names, [PathBuf::from("/m/1.mp3"), PathBuf::from("/m/2.mp3")]);
        let reordered = s.load_playlists(&[kb.clone()]);
        assert_eq!(reordered[0].key, kb);
        s.delete_playlist(&ka).unwrap();
        assert_eq!(s.load_playlists(&[]).len(), 1);
    }

    #[test]
    fn broken_files_are_set_aside_and_reported() {
        let (_d, mut s) = store();
        fs::create_dir_all(&s.paths.config).unwrap();
        fs::write(s.paths.config.join("settings.json"), b"{ not json").unwrap();
        let settings = s.load_settings();
        assert_eq!(settings.volume, Settings::default().volume);
        assert_eq!(s.load_warnings.len(), 1);
        assert!(s.paths.config.join("settings.json.broken").exists());
    }

    #[test]
    fn settings_round_trip_with_unknown_and_missing_fields() {
        let (_d, mut s) = store();
        fs::create_dir_all(&s.paths.config).unwrap();
        fs::write(s.paths.config.join("settings.json"), br#"{"volume":0.25,"future_field":1}"#).unwrap();
        let mut settings = s.load_settings();
        assert_eq!(settings.volume, 0.25);
        assert!(!settings.resume_playback);
        settings.shuffle = true;
        s.save_settings(&settings).unwrap();
        assert!(s.load_settings().shuffle);
    }

    #[test]
    fn setlists_by_fingerprint() {
        let (_d, s) = store();
        let mut setlist = Setlist::default();
        setlist.entries.push(SetlistEntry::new("A", "B", Some(1.0), Provenance::Manual));
        let saved = SavedSetlist { fingerprint: "ab-12".into(), last_path: "/x.mp3".into(), setlist };
        save_setlist(&s.paths, "ab-12", Some(&saved)).unwrap();
        let found = find_setlist(&s.paths, "ab-12", Path::new("/elsewhere.mp3")).unwrap();
        assert_eq!(found.setlist.entries[0].artist, "A");
        assert!(find_setlist(&s.paths, "ffff", Path::new("/x.mp3")).is_some(), "path fallback");
        assert!(find_setlist(&s.paths, "../../etc/passwd", Path::new("/nope.mp3")).is_none());
        assert!(save_setlist(&s.paths, "../x", Some(&saved)).is_err());
        save_setlist(&s.paths, "ab-12", None).unwrap();
        assert!(find_setlist(&s.paths, "ab-12", Path::new("/x.mp3")).is_none());
    }

    #[test]
    fn fingerprint_survives_retagging() {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures");
        let dir = tempfile::tempdir().unwrap();
        let mp3 = dir.path().join("mix.mp3");
        fs::copy(fixtures.join("mix_chapters.mp3"), &mp3).unwrap();
        let before = fingerprint(&mp3).unwrap();
        let mut edit = nullamp_meta::Edit::default();
        edit.fields.insert(nullamp_meta::Field::Title, Some("A much longer title than before, to grow the tag".into()));
        nullamp_meta::write(&mp3, nullamp_meta::stamp(&mp3).unwrap(), &edit, &Default::default()).unwrap();
        assert_eq!(fingerprint(&mp3).unwrap(), before);
    }

    #[test]
    fn fingerprint_follows_content_not_path() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.mp3");
        let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        fs::write(&a, &data).unwrap();
        let b = dir.path().join("renamed.mp3");
        fs::copy(&a, &b).unwrap();
        assert_eq!(fingerprint(&a).unwrap(), fingerprint(&b).unwrap());
        let mut changed = data.clone();
        changed[199_990] ^= 1;
        fs::write(&b, &changed).unwrap();
        assert_ne!(fingerprint(&a).unwrap(), fingerprint(&b).unwrap());
        let small = dir.path().join("small.mp3");
        fs::write(&small, b"tiny").unwrap();
        assert!(fingerprint(&small).is_ok());
    }
}
