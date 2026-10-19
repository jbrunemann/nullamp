//! Disk cache of waveform overviews, so a mix is only decoded for its waveform once.
//!
//! Entries are keyed by the file's content fingerprint (see `store::fingerprint`), which
//! survives retagging and renaming. A hit refreshes the entry's modified time, so the
//! age-based cleanup removes what has gone unused, not what is merely old.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use nullamp_audio::Waveform;

/// Entries not used for this long are removed at startup. A few KB each; this only keeps
/// the folder from collecting every file ever played.
pub const MAX_AGE: Duration = Duration::from_secs(90 * 24 * 3600);
/// A hit only rewrites the modified time when it is older than this, so replaying a track does not touch the disk.
const TOUCH_AFTER: Duration = Duration::from_secs(24 * 3600);

fn entry(dir: &Path, fingerprint: &str) -> std::path::PathBuf {
    dir.join(format!("{:016x}.wf", nullamp_online::fnv1a(fingerprint.as_bytes())))
}

pub fn load(dir: &Path, fingerprint: &str) -> Option<Waveform> {
    let path = entry(dir, fingerprint);
    let wave = Waveform::from_bytes(&fs::read(&path).ok()?)?;
    let stale = fs::metadata(&path).ok()?.modified().ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > TOUCH_AFTER);
    if stale && let Ok(f) = fs::File::options().write(true).open(&path) {
        let _ = f.set_modified(SystemTime::now());
    }
    Some(wave)
}

pub fn store(dir: &Path, fingerprint: &str, wave: &Waveform) {
    if fs::create_dir_all(dir).is_err() {
        return;
    }
    let path = entry(dir, fingerprint);
    let tmp = path.with_extension("tmp");
    if fs::write(&tmp, wave.to_bytes()).is_ok() && fs::rename(&tmp, &path).is_err() {
        let _ = fs::remove_file(&tmp);
    }
}

/// Remove entries unused for `max_age`. Returns how many went.
pub fn prune(dir: &Path, max_age: Duration) -> usize {
    nullamp_online::prune_cache(dir, max_age)
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
