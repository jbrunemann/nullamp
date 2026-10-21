//! Disk cache of covers found online for the tracks inside a mix, so each track is looked up
//! once in a while and not on every play.
//!
//! Entries are keyed by the track's artist and title (see [`key`]): the same track in another
//! mix finds the same cover. A found cover is kept as the image file itself, which also serves
//! the OS Now Playing overlay; "nothing found" is kept as a marker holding the matcher's
//! version, so a better matcher asks again. Both expire, since
//! the services add releases and covers: misses sooner than hits.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// A found cover is used for this long before it is looked up again.
pub const HIT_AGE: Duration = Duration::from_secs(30 * 24 * 3600);
/// "Nothing found" is believed for this long.
pub const MISS_AGE: Duration = Duration::from_secs(7 * 24 * 3600);

const MISS: &str = "none";

pub enum Cached {
    Image(PathBuf),
    Nothing,
}

/// The cache key of a track: its artist and title, normalized so spelling details
/// ("Keepin' On", "keepin on") do not count.
pub fn key(artist: &str, title: &str) -> String {
    format!("{}\t{}", nullamp_artwork::normalize(artist), nullamp_artwork::normalize(title))
}

fn entry(dir: &Path, key: &str, extension: &str) -> PathBuf {
    dir.join(format!("{:016x}.{extension}", crate::store::fnv1a(key.as_bytes())))
}

fn age(path: &Path) -> Option<Duration> {
    fs::metadata(path).ok()?.modified().ok()?.elapsed().ok()
}

/// What the cache knows about `key`, `None` when it knows nothing or that has expired.
pub fn load(dir: &Path, key: &str) -> Option<Cached> {
    for extension in ["jpg", "png"] {
        let path = entry(dir, key, extension);
        if age(&path).is_some_and(|a| a < HIT_AGE) {
            return Some(Cached::Image(path));
        }
    }
    let miss = entry(dir, key, MISS);
    let current = fs::read_to_string(&miss).is_ok_and(|v| v.trim() == nullamp_artwork::MATCHER_VERSION.to_string());
    (current && age(&miss).is_some_and(|a| a < MISS_AGE)).then_some(Cached::Nothing)
}

/// Keep the cover found for `key` and return where it is.
pub fn store_image(dir: &Path, key: &str, bytes: &[u8]) -> Option<PathBuf> {
    let extension = if bytes.starts_with(b"\x89PNG") { "png" } else { "jpg" };
    let path = entry(dir, key, extension);
    write(&path, bytes).then(|| {
        let _ = fs::remove_file(entry(dir, key, MISS));
        path
    })
}

/// Remember that nothing was found for `key`.
pub fn store_nothing(dir: &Path, key: &str) {
    write(&entry(dir, key, MISS), nullamp_artwork::MATCHER_VERSION.to_string().as_bytes());
}

fn write(path: &Path, bytes: &[u8]) -> bool {
    let Some(dir) = path.parent() else { return false };
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let ok = fs::create_dir_all(dir).is_ok() && fs::write(&tmp, bytes).is_ok() && fs::rename(&tmp, path).is_ok();
    if !ok {
        let _ = fs::remove_file(&tmp);
    }
    ok
}

/// Remove expired entries. Returns how many went.
pub fn prune(dir: &Path) -> usize {
    let Ok(read) = fs::read_dir(dir) else { return 0 };
    let now = SystemTime::now();
    read.filter_map(|e| e.ok())
        .filter(|e| {
            let path = e.path();
            let limit = if path.extension().is_some_and(|x| x == MISS) { MISS_AGE } else { HIT_AGE };
            e.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| now.duration_since(t).ok()).is_some_and(|a| a > limit)
        })
        .filter(|e| fs::remove_file(e.path()).is_ok())
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backdate(path: &Path, by: Duration) {
        fs::File::options().write(true).open(path).unwrap().set_modified(SystemTime::now() - by).unwrap();
    }

    #[test]
    fn keys_ignore_spelling_details() {
        assert_eq!(key("Tino & Malachy", "Keepin' On"), key("tino  malachy", "Keepin On"));
        assert_ne!(key("A", "B"), key("B", "A"));
    }

    #[test]
    fn stores_and_loads_covers_and_misses() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(dir.path(), "a").is_none());
        store_nothing(dir.path(), "a");
        assert!(matches!(load(dir.path(), "a"), Some(Cached::Nothing)));
        fs::write(entry(dir.path(), "a", MISS), b"").unwrap();
        assert!(load(dir.path(), "a").is_none(), "a miss from an older matcher is asked again");
        store_nothing(dir.path(), "a");
        let path = store_image(dir.path(), "a", b"\x89PNG....").unwrap();
        assert_eq!(path.extension().unwrap(), "png");
        assert!(matches!(load(dir.path(), "a"), Some(Cached::Image(p)) if p == path), "a cover replaces the miss");
        assert!(load(dir.path(), "b").is_none());
    }

    #[test]
    fn entries_expire() {
        let dir = tempfile::tempdir().unwrap();
        let hit = store_image(dir.path(), "hit", b"jpeg").unwrap();
        store_nothing(dir.path(), "miss");
        backdate(&entry(dir.path(), "miss", MISS), MISS_AGE + Duration::from_secs(60));
        assert!(load(dir.path(), "miss").is_none());
        assert!(load(dir.path(), "hit").is_some());
        backdate(&hit, HIT_AGE + Duration::from_secs(60));
        assert!(load(dir.path(), "hit").is_none());
        assert_eq!(prune(dir.path()), 2);
    }
}
