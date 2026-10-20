//! What the cache folder holds and clearing it, by kind or entirely.
//!
//! Everything in it can be recomputed or fetched again, so clearing only costs time: the
//! analysis runs again when a track plays, covers are looked up again when a track of a mix
//! becomes active. The one exception is what was copied from a rekordbox USB stick, which cannot
//! be read again while the stick is away.

use std::fs;
use std::path::{Path, PathBuf};

use crate::paths::AppPaths;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Waveforms,
    Tempos,
    Rekordbox,
    TrackCovers,
    NowPlaying,
    /// Anything no current feature writes, such as the files of removed features.
    Leftovers,
}

impl Kind {
    pub const ALL: [Kind; 6] = [Kind::Waveforms, Kind::Tempos, Kind::Rekordbox, Kind::TrackCovers, Kind::NowPlaying, Kind::Leftovers];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Waveforms => "Waveforms",
            Kind::Tempos => "Tempo (BPM)",
            Kind::Rekordbox => "rekordbox data",
            Kind::TrackCovers => "Mix track covers",
            Kind::NowPlaying => "Now Playing covers",
            Kind::Leftovers => "Leftovers",
        }
    }

    /// What it is and what clearing it costs, for the tooltip.
    pub fn about(self) -> &'static str {
        match self {
            Kind::Waveforms => "Waveform overviews of decoded files. Made again the next time a track plays.",
            Kind::Tempos => "Detected tempos, including \"no clear beat\". Detected again when a track plays. Tempos already saved in a playlist stay.",
            Kind::Rekordbox => "Waveforms, key and BPM copied from rekordbox USB sticks. Read again when a file plays from its stick, so files whose stick is away lose them.",
            Kind::TrackCovers => "Covers found online for the tracks inside mixes, and the tracks nothing was found for. Looked up again when a track becomes active.",
            Kind::NowPlaying => "Cover images handed to the macOS Now Playing overlay. Written again when a track plays.",
            Kind::Leftovers => "Files nothing reads any more, such as those of removed features.",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub entries: usize,
    pub bytes: u64,
}

fn classify(cache: &Path, path: &Path) -> Kind {
    let Ok(rel) = path.strip_prefix(cache) else { return Kind::Leftovers };
    let parts: Vec<&str> = rel.iter().filter_map(|p| p.to_str()).collect();
    let extension = path.extension().and_then(|e| e.to_str());
    match (parts.as_slice(), extension) {
        (["waveforms", _], Some("wf")) => Kind::Waveforms,
        (["waveforms", _], Some("bpm")) => Kind::Tempos,
        (["waveforms", _], Some("rwf" | "rbi")) => Kind::Rekordbox,
        (["artwork", "tracks", _], _) => Kind::TrackCovers,
        (["artwork", _], _) => Kind::NowPlaying,
        _ => Kind::Leftovers,
    }
}

/// Every file under `dir` with its size; symlinks are listed, never followed.
fn files(dir: &Path, out: &mut Vec<(PathBuf, u64)>) {
    let Ok(read) = fs::read_dir(dir) else { return };
    for entry in read.filter_map(|e| e.ok()) {
        let Ok(meta) = fs::symlink_metadata(entry.path()) else { continue };
        if meta.is_dir() {
            files(&entry.path(), out);
        } else {
            out.push((entry.path(), meta.len()));
        }
    }
}

/// How many entries and bytes each kind holds, in `Kind::ALL` order.
pub fn scan(paths: &AppPaths) -> Vec<(Kind, Usage)> {
    let mut found = Vec::new();
    files(&paths.cache, &mut found);
    Kind::ALL
        .iter()
        .map(|&kind| {
            let mut usage = Usage::default();
            for (_, bytes) in found.iter().filter(|(p, _)| classify(&paths.cache, p) == kind) {
                usage.entries += 1;
                usage.bytes += bytes;
            }
            (kind, usage)
        })
        .collect()
}

/// Remove the files of `kinds`. Returns how many went.
pub fn clear(paths: &AppPaths, kinds: &[Kind]) -> usize {
    let mut found = Vec::new();
    files(&paths.cache, &mut found);
    let removed = found.iter().filter(|(p, _)| kinds.contains(&classify(&paths.cache, p))).filter(|(p, _)| fs::remove_file(p).is_ok()).count();
    if kinds.contains(&Kind::Leftovers) {
        remove_empty_dirs(&paths.cache, true);
    }
    removed
}

/// Folders left empty by `clear`; `keep` spares `dir` itself. The writers create their folders again.
fn remove_empty_dirs(dir: &Path, keep: bool) {
    let Ok(read) = fs::read_dir(dir) else { return };
    for entry in read.filter_map(|e| e.ok()) {
        if fs::symlink_metadata(entry.path()).is_ok_and(|m| m.is_dir()) {
            remove_empty_dirs(&entry.path(), false);
        }
    }
    if !keep {
        let _ = fs::remove_dir(dir);
    }
}

/// "1.4 MB": decimal units, like Finder.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["bytes", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} bytes") } else { format!("{value:.1} {}", UNITS[unit]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(dir: &Path) -> AppPaths {
        AppPaths { config: dir.join("config"), data: dir.join("data"), cache: dir.join("cache") }
    }

    fn put(paths: &AppPaths, rel: &str, len: usize) {
        let path = paths.cache.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![0u8; len]).unwrap();
    }

    fn usage(paths: &AppPaths, kind: Kind) -> Usage {
        scan(paths).into_iter().find(|(k, _)| *k == kind).unwrap().1
    }

    fn seeded(dir: &Path) -> AppPaths {
        let paths = paths(dir);
        for (rel, len) in [
            ("waveforms/a.wf", 10),
            ("waveforms/b.wf", 20),
            ("waveforms/a.bpm", 12),
            ("waveforms/a.rwf", 5),
            ("waveforms/a.rbi", 7),
            ("artwork/tracks/a.jpg", 100),
            ("artwork/tracks/b.none", 1),
            ("artwork/now-a.jpg", 50),
            ("online/0240/page.html", 9),
            ("stray.txt", 3),
        ] {
            put(&paths, rel, len);
        }
        paths
    }

    #[test]
    fn counts_entries_and_bytes_per_kind() {
        let dir = tempfile::tempdir().unwrap();
        let paths = seeded(dir.path());
        assert_eq!(usage(&paths, Kind::Waveforms), Usage { entries: 2, bytes: 30 });
        assert_eq!(usage(&paths, Kind::Tempos), Usage { entries: 1, bytes: 12 });
        assert_eq!(usage(&paths, Kind::Rekordbox), Usage { entries: 2, bytes: 12 });
        assert_eq!(usage(&paths, Kind::TrackCovers), Usage { entries: 2, bytes: 101 });
        assert_eq!(usage(&paths, Kind::NowPlaying), Usage { entries: 1, bytes: 50 });
        assert_eq!(usage(&paths, Kind::Leftovers), Usage { entries: 2, bytes: 12 });
    }

    #[test]
    fn a_missing_cache_folder_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(scan(&paths(dir.path())).iter().all(|(_, u)| *u == Usage::default()));
    }

    #[test]
    fn clears_only_the_chosen_kinds() {
        let dir = tempfile::tempdir().unwrap();
        let paths = seeded(dir.path());
        assert_eq!(clear(&paths, &[Kind::Tempos, Kind::NowPlaying]), 2);
        assert_eq!(usage(&paths, Kind::Tempos), Usage::default());
        assert_eq!(usage(&paths, Kind::NowPlaying), Usage::default());
        assert_eq!(usage(&paths, Kind::Waveforms).entries, 2);
        assert_eq!(usage(&paths, Kind::Leftovers).entries, 2);
    }

    #[test]
    fn clearing_leftovers_removes_their_folders_but_not_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let paths = seeded(dir.path());
        clear(&paths, &[Kind::Leftovers]);
        assert!(!paths.cache.join("online").exists());
        assert!(paths.cache.join("waveforms").is_dir(), "folders of live caches stay");
        assert!(paths.cache.is_dir());
    }

    #[test]
    fn clearing_everything_leaves_nothing_counted() {
        let dir = tempfile::tempdir().unwrap();
        let paths = seeded(dir.path());
        assert_eq!(clear(&paths, &Kind::ALL), 10);
        assert!(scan(&paths).iter().all(|(_, u)| *u == Usage::default()));
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(format_bytes(0), "0 bytes");
        assert_eq!(format_bytes(999), "999 bytes");
        assert_eq!(format_bytes(1_500), "1.5 KB");
        assert_eq!(format_bytes(29_400_000), "29.4 MB");
    }
}
