//! Disk cache of what the background analysis finds in a file (waveform overview, tempo), so
//! a file is only decoded for it once.
//!
//! Entries are keyed by the file's content fingerprint (see `store::fingerprint`), which
//! survives retagging and renaming. A hit refreshes the entry's modified time, so the
//! age-based cleanup removes what has gone unused, not what is merely old.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use nullamp_audio::{Tempo, Waveform};

/// Entries not used for this long are removed at startup. A few KB each; this only keeps
/// the folder from collecting every file ever played.
pub const MAX_AGE: Duration = Duration::from_secs(90 * 24 * 3600);
/// A hit only rewrites the modified time when it is older than this, so replaying a track does not touch the disk.
const TOUCH_AFTER: Duration = Duration::from_secs(24 * 3600);

fn entry(dir: &Path, fingerprint: &str) -> std::path::PathBuf {
    entry_as(dir, fingerprint, "wf")
}

fn entry_as(dir: &Path, fingerprint: &str, extension: &str) -> std::path::PathBuf {
    dir.join(format!("{:016x}.{extension}", crate::store::fnv1a(fingerprint.as_bytes())))
}

/// The entry's bytes, refreshing its modified time when it is getting old.
fn read_entry(path: &Path) -> Option<Vec<u8>> {
    let bytes = fs::read(path).ok()?;
    let stale = fs::metadata(path).ok()?.modified().ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > TOUCH_AFTER);
    if stale && let Ok(f) = fs::File::options().write(true).open(path) {
        let _ = f.set_modified(SystemTime::now());
    }
    Some(bytes)
}

fn write_entry(path: &Path, bytes: &[u8]) {
    let Some(dir) = path.parent() else { return };
    if fs::create_dir_all(dir).is_err() {
        return;
    }
    let tmp = path.with_extension("tmp");
    if fs::write(&tmp, bytes).is_ok() && fs::rename(&tmp, path).is_err() {
        let _ = fs::remove_file(&tmp);
    }
}

pub fn load(dir: &Path, fingerprint: &str) -> Option<Waveform> {
    Waveform::from_bytes(&read_entry(&entry(dir, fingerprint))?)
}

pub fn store(dir: &Path, fingerprint: &str, wave: &Waveform) {
    write_entry(&entry(dir, fingerprint), &wave.to_bytes());
}

const TEMPO_MAGIC: &[u8; 4] = b"NBP1";

/// `Some(None)` is a cached "no clear beat", so such a file is not analysed again; `None` is a miss.
pub fn load_tempo(dir: &Path, fingerprint: &str) -> Option<Option<Tempo>> {
    let bytes = read_entry(&entry_as(dir, fingerprint, "bpm"))?;
    let [m0, m1, m2, m3, b0, b1, b2, b3, c0, c1, c2, c3] = bytes[..] else { return None };
    if &[m0, m1, m2, m3] != TEMPO_MAGIC {
        return None;
    }
    let (bpm, confidence) = (f32::from_le_bytes([b0, b1, b2, b3]), f32::from_le_bytes([c0, c1, c2, c3]));
    Some((bpm > 0.0).then_some(Tempo { bpm, confidence }))
}

pub fn store_tempo(dir: &Path, fingerprint: &str, tempo: Option<Tempo>) {
    let (bpm, confidence) = tempo.map_or((0.0, 0.0), |t| (t.bpm, t.confidence));
    let mut bytes = TEMPO_MAGIC.to_vec();
    bytes.extend_from_slice(&bpm.to_le_bytes());
    bytes.extend_from_slice(&confidence.to_le_bytes());
    write_entry(&entry_as(dir, fingerprint, "bpm"), &bytes);
}

/// Remove entries unused for `max_age`. Returns how many went.
pub fn prune(dir: &Path, max_age: Duration) -> usize {
    let Ok(read) = fs::read_dir(dir) else { return 0 };
    let now = SystemTime::now();
    read.filter_map(|e| e.ok())
        .filter(|e| {
            e.metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| now.duration_since(t).ok())
                .is_some_and(|age| age > max_age)
        })
        .filter(|e| fs::remove_file(e.path()).is_ok())
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wave() -> Waveform {
        Waveform { peak: vec![9, 8, 7], rms: vec![3, 2, 1] }
    }

    #[test]
    fn stores_and_loads_by_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path(), "abc"), None);
        store(dir.path(), "abc", &wave());
        assert_eq!(load(dir.path(), "abc"), Some(wave()));
        assert_eq!(load(dir.path(), "abd"), None);
    }

    #[test]
    fn tempo_round_trips_including_no_tempo() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_tempo(dir.path(), "abc"), None);
        let tempo = Tempo { bpm: 127.5, confidence: 0.8 };
        store_tempo(dir.path(), "abc", Some(tempo));
        store_tempo(dir.path(), "none", None);
        assert_eq!(load_tempo(dir.path(), "abc"), Some(Some(tempo)));
        assert_eq!(load_tempo(dir.path(), "none"), Some(None));
        fs::write(entry_as(dir.path(), "abc", "bpm"), b"NBP1xx").unwrap();
        assert_eq!(load_tempo(dir.path(), "abc"), None, "damage is a miss");
    }

    #[test]
    fn damaged_entry_is_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        store(dir.path(), "abc", &wave());
        fs::write(entry(dir.path(), "abc"), b"garbage").unwrap();
        assert_eq!(load(dir.path(), "abc"), None);
    }

    #[test]
    fn prune_removes_unused_entries_and_a_hit_keeps_one_alive() {
        let dir = tempfile::tempdir().unwrap();
        store(dir.path(), "old", &wave());
        store(dir.path(), "used", &wave());
        let long_ago = SystemTime::now() - Duration::from_secs(100 * 24 * 3600);
        for name in ["old", "used"] {
            fs::File::options().write(true).open(entry(dir.path(), name)).unwrap().set_modified(long_ago).unwrap();
        }
        assert!(load(dir.path(), "used").is_some(), "a hit refreshes the entry");
        assert_eq!(prune(dir.path(), MAX_AGE), 1);
        assert_eq!(load(dir.path(), "old"), None);
        assert!(load(dir.path(), "used").is_some());
    }
}
