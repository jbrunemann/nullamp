use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ids::EntryId;
use crate::track::TrackInfo;

/// What is known about whether an entry can be played.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum EntryStatus {
    /// Not checked yet.
    #[default]
    Unknown,
    Ok,
    /// The file does not exist (deleted, moved, or on an unmounted volume).
    Missing,
    /// The file exists but could not be decoded.
    Unsupported(String),
}

impl EntryStatus {
    pub fn is_playable(&self) -> bool {
        matches!(self, EntryStatus::Unknown | EntryStatus::Ok)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlaylistEntry {
    #[serde(skip, default)]
    pub id: EntryId,
    pub path: PathBuf,
    #[serde(default)]
    pub info: TrackInfo,
    #[serde(skip, default)]
    pub status: EntryStatus,
}

impl PlaylistEntry {
    pub fn new(path: PathBuf) -> Self {
        PlaylistEntry { id: EntryId::new(), path, info: TrackInfo::default(), status: EntryStatus::Unknown }
    }

    pub fn with_info(path: PathBuf, info: TrackInfo) -> Self {
        PlaylistEntry { info, ..Self::new(path) }
    }
}

/// An ordered list of audio files.
///
/// Entries are addressed by [`EntryId`] rather than index so that selection and the
/// current track stay correct across edits.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Playlist {
    pub name: String,
    entries: Vec<PlaylistEntry>,
}

impl Playlist {
    pub fn new(name: impl Into<String>) -> Self {
        Playlist { name: name.into(), entries: Vec::new() }
    }

    pub fn entries(&self) -> &[PlaylistEntry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn ids(&self) -> impl Iterator<Item = EntryId> + '_ {
        self.entries.iter().map(|e| e.id)
    }

    pub fn index_of(&self, id: EntryId) -> Option<usize> {
        self.entries.iter().position(|e| e.id == id)
    }

    pub fn get(&self, id: EntryId) -> Option<&PlaylistEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    pub fn get_mut(&mut self, id: EntryId) -> Option<&mut PlaylistEntry> {
        self.entries.iter_mut().find(|e| e.id == id)
    }

    pub fn entries_with_path_mut<'a>(&'a mut self, path: &'a Path) -> impl Iterator<Item = &'a mut PlaylistEntry> {
        self.entries.iter_mut().filter(move |e| e.path == path)
    }

    pub fn push(&mut self, entry: PlaylistEntry) -> EntryId {
        let id = entry.id;
        self.entries.push(entry);
        id
    }

    /// Insert entries at `index` (clamped to the end). Returns their ids in order.
    pub fn insert(&mut self, index: usize, entries: Vec<PlaylistEntry>) -> Vec<EntryId> {
        let index = index.min(self.entries.len());
        let ids = entries.iter().map(|e| e.id).collect();
        self.entries.splice(index..index, entries);
        ids
    }

    pub fn add_paths(&mut self, paths: impl IntoIterator<Item = PathBuf>) -> Vec<EntryId> {
        let entries: Vec<_> = paths.into_iter().map(PlaylistEntry::new).collect();
        self.insert(self.entries.len(), entries)
    }

    /// Remove the given entries. Returns how many were removed.
    pub fn remove(&mut self, ids: &HashSet<EntryId>) -> usize {
        let before = self.entries.len();
        self.entries.retain(|e| !ids.contains(&e.id));
        before - self.entries.len()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Move each selected entry one step up (`delta < 0`) or down (`delta > 0`).
    ///
    /// Blocks of selected entries move together; a block already at the edge stays put
    /// and the rest keep their relative order. Returns whether anything moved.
    pub fn move_by(&mut self, ids: &HashSet<EntryId>, delta: isize) -> bool {
        let mut moved = false;
        let steps = delta.unsigned_abs();
        for _ in 0..steps {
            let n = self.entries.len();
            if delta < 0 {
                for i in 1..n {
                    if ids.contains(&self.entries[i].id) && !ids.contains(&self.entries[i - 1].id) {
                        self.entries.swap(i, i - 1);
                        moved = true;
                    }
                }
            } else {
                for i in (0..n.saturating_sub(1)).rev() {
                    if ids.contains(&self.entries[i].id) && !ids.contains(&self.entries[i + 1].id) {
                        self.entries.swap(i, i + 1);
                        moved = true;
                    }
                }
            }
        }
        moved
    }

    /// Move the selected entries so they form a contiguous block that starts at the
    /// gap before `target` (an index into the list *before* the move, `len` = end).
    /// This is the drag-and-drop operation. Relative order is preserved.
    pub fn move_to(&mut self, ids: &HashSet<EntryId>, target: usize) -> bool {
        let target = target.min(self.entries.len());
        let before: Vec<EntryId> = self.ids().collect();
        let removed_before_target = self.entries[..target].iter().filter(|e| ids.contains(&e.id)).count();
        let (picked, mut rest): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.entries).into_iter().partition(|e| ids.contains(&e.id));
        let insert_at = target - removed_before_target;
        rest.splice(insert_at..insert_at, picked);
        self.entries = rest;
        !self.ids().eq(before)
    }

    /// Stable sort by `compare`.
    pub fn sort_by(&mut self, compare: impl FnMut(&PlaylistEntry, &PlaylistEntry) -> std::cmp::Ordering) {
        self.entries.sort_by(compare);
    }

    pub fn reverse(&mut self) {
        self.entries.reverse();
    }

    /// Sum of known durations, and whether any entry's duration is unknown.
    pub fn total_duration(&self) -> (f64, bool) {
        let mut total = 0.0;
        let mut incomplete = false;
        for e in &self.entries {
            match e.info.duration {
                Some(d) if d.is_finite() && d > 0.0 => total += d,
                _ => incomplete = true,
            }
        }
        (total, incomplete)
    }

    /// Ids of the entries between two ids, inclusive, in list order (for shift-click).
    pub fn range(&self, a: EntryId, b: EntryId) -> Vec<EntryId> {
        match (self.index_of(a), self.index_of(b)) {
            (Some(i), Some(j)) => {
                let (lo, hi) = if i <= j { (i, j) } else { (j, i) };
                self.entries[lo..=hi].iter().map(|e| e.id).collect()
            }
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(names: &[&str]) -> Playlist {
        let mut p = Playlist::new("test");
        p.add_paths(names.iter().map(PathBuf::from));
        p
    }

    fn names(p: &Playlist) -> Vec<String> {
        p.entries().iter().map(|e| e.path.display().to_string()).collect()
    }

    fn ids_of(p: &Playlist, wanted: &[&str]) -> HashSet<EntryId> {
        p.entries().iter().filter(|e| wanted.contains(&e.path.to_str().unwrap())).map(|e| e.id).collect()
    }

    #[test]
    fn add_insert_remove_clear() {
        let mut p = list(&["a", "b", "c"]);
        p.insert(1, vec![PlaylistEntry::new("x".into())]);
        assert_eq!(names(&p), ["a", "x", "b", "c"]);
        p.insert(99, vec![PlaylistEntry::new("z".into())]);
        assert_eq!(names(&p), ["a", "x", "b", "c", "z"]);
        let sel = ids_of(&p, &["x", "c"]);
        assert_eq!(p.remove(&sel), 2);
        assert_eq!(names(&p), ["a", "b", "z"]);
        p.clear();
        assert!(p.is_empty());
    }

    #[test]
    fn ids_survive_reordering() {
        let mut p = list(&["a", "b", "c"]);
        let c = p.entries()[2].id;
        p.move_to(&HashSet::from([c]), 0);
        assert_eq!(p.index_of(c), Some(0));
        assert_eq!(p.get(c).unwrap().path, PathBuf::from("c"));
    }

    #[test]
    fn move_up_and_down_blocks() {
        let mut p = list(&["a", "b", "c", "d", "e"]);
        let sel = ids_of(&p, &["b", "d"]);
        assert!(p.move_by(&sel, -1));
        assert_eq!(names(&p), ["b", "a", "d", "c", "e"]);
        // "b" is at the top and stays, "d" keeps moving.
        assert!(p.move_by(&sel, -1));
        assert_eq!(names(&p), ["b", "d", "a", "c", "e"]);
        assert!(!p.move_by(&sel, -1));
        assert!(p.move_by(&sel, 3));
        assert_eq!(names(&p), ["a", "c", "e", "b", "d"]);
        assert!(!p.move_by(&sel, 1));
    }

    #[test]
    fn drag_drop_move_to() {
        let mut p = list(&["a", "b", "c", "d", "e"]);
        let sel = ids_of(&p, &["a", "c"]);
        // Drop into the gap before "e" (index 4 in the original list).
        p.move_to(&sel, 4);
        assert_eq!(names(&p), ["b", "d", "a", "c", "e"]);
        let sel = ids_of(&p, &["e"]);
        p.move_to(&sel, 0);
        assert_eq!(names(&p), ["e", "b", "d", "a", "c"]);
        let sel = ids_of(&p, &["e", "d"]);
        p.move_to(&sel, 5);
        assert_eq!(names(&p), ["b", "a", "c", "e", "d"]);
    }

    #[test]
    fn sort_and_reverse() {
        let mut p = list(&["b", "c", "a"]);
        p.sort_by(|x, y| x.path.cmp(&y.path));
        assert_eq!(names(&p), ["a", "b", "c"]);
        p.reverse();
        assert_eq!(names(&p), ["c", "b", "a"]);
    }

    #[test]
    fn range_selection() {
        let p = list(&["a", "b", "c", "d"]);
        let ids: Vec<_> = p.ids().collect();
        assert_eq!(p.range(ids[3], ids[1]), vec![ids[1], ids[2], ids[3]]);
    }

    #[test]
    fn total_duration_reports_unknowns() {
        let mut p = Playlist::new("t");
        p.push(PlaylistEntry::with_info("a".into(), TrackInfo { duration: Some(60.0), ..Default::default() }));
        p.push(PlaylistEntry::new("b".into()));
        assert_eq!(p.total_duration(), (60.0, true));
    }

    #[test]
    fn serde_round_trip_regenerates_ids() {
        let p = list(&["a", "b"]);
        let json = serde_json::to_string(&p).unwrap();
        let q: Playlist = serde_json::from_str(&json).unwrap();
        assert_eq!(names(&q), ["a", "b"]);
        assert_ne!(q.entries()[0].id, q.entries()[1].id);
        assert_ne!(q.entries()[0].id, p.entries()[0].id);
    }
}
