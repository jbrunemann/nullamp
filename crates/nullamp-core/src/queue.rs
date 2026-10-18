//! Play order: which entry comes next given shuffle and repeat settings.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::ids::EntryId;
use crate::playlist::{Playlist, PlaylistEntry};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepeatMode {
    #[default]
    Off,
    /// Repeat the current track when it ends. Explicit next/previous still move.
    One,
    /// Wrap around at the end of the playlist.
    All,
}

impl RepeatMode {
    pub fn cycle(self) -> Self {
        match self {
            RepeatMode::Off => RepeatMode::All,
            RepeatMode::All => RepeatMode::One,
            RepeatMode::One => RepeatMode::Off,
        }
    }
}

/// Decides the next and previous entries.
///
/// In shuffle mode a random permutation is kept so that "previous" retraces the
/// shuffled order and every entry plays once per cycle. Entries added while shuffling
/// are inserted at random positions *after* the current one, so they still get played.
#[derive(Clone, Debug)]
pub struct Navigator {
    shuffle: bool,
    pub repeat: RepeatMode,
    order: Vec<EntryId>,
    /// Position of the current entry in the active sequence, remembered so that
    /// navigation continues sensibly after the current entry is removed.
    anchor: Option<usize>,
    rng: u64,
}

impl Navigator {
    pub fn new(seed: u64) -> Self {
        Navigator { shuffle: false, repeat: RepeatMode::Off, order: Vec::new(), anchor: None, rng: seed | 1 }
    }

    pub fn shuffle(&self) -> bool {
        self.shuffle
    }

    pub fn set_shuffle(&mut self, on: bool, playlist: &Playlist, current: Option<EntryId>) {
        self.shuffle = on;
        if on {
            self.reshuffle(playlist, current);
        } else {
            self.order.clear();
        }
        self.sync(playlist, current);
    }

    /// New permutation, with the current entry first so playback continues naturally.
    pub fn reshuffle(&mut self, playlist: &Playlist, current: Option<EntryId>) {
        let mut ids: Vec<EntryId> = playlist.ids().filter(|id| Some(*id) != current).collect();
        for i in (1..ids.len()).rev() {
            let j = (self.next_random() % (i as u64 + 1)) as usize;
            ids.swap(i, j);
        }
        if let Some(c) = current.filter(|c| playlist.index_of(*c).is_some()) {
            ids.insert(0, c);
        }
        self.order = ids;
        self.anchor = current.and_then(|c| self.order.iter().position(|id| *id == c));
    }

    /// Reconcile with the playlist after edits and record the current entry's position.
    /// Call after every playlist change and whenever the current entry changes.
    pub fn sync(&mut self, playlist: &Playlist, current: Option<EntryId>) {
        if self.shuffle {
            let present: HashSet<EntryId> = playlist.ids().collect();
            // Keep the anchor meaningful when entries before it are removed.
            if let Some(a) = self.anchor {
                let removed_before = self.order[..a.min(self.order.len())].iter().filter(|id| !present.contains(id)).count();
                self.anchor = Some(a - removed_before);
            }
            self.order.retain(|id| present.contains(id));
            let known: HashSet<EntryId> = self.order.iter().copied().collect();
            let new: Vec<EntryId> = playlist.ids().filter(|id| !known.contains(id)).collect();
            for id in new {
                let after = current
                    .and_then(|c| self.order.iter().position(|x| *x == c))
                    .map(|p| p + 1)
                    .or(self.anchor)
                    .unwrap_or(0)
                    .min(self.order.len());
                let span = (self.order.len() - after + 1) as u64;
                let at = after + (self.next_random() % span) as usize;
                self.order.insert(at, id);
            }
        }
        if let Some(pos) = current.and_then(|c| self.position(playlist, c)) {
            self.anchor = Some(pos);
        } else if let Some(a) = self.anchor {
            // The current entry is gone. Keep pointing at the gap it left: the entry
            // that slid into its place is "next".
            self.anchor = Some(a.min(self.sequence_len(playlist)));
        }
    }

    /// The entry to play after `current`.
    ///
    /// `auto` is true when the current track ended by itself (repeat-one applies);
    /// false for an explicit "next" command. `playable` filters out entries known to
    /// be missing or broken.
    pub fn next(
        &mut self,
        playlist: &Playlist,
        current: Option<EntryId>,
        auto: bool,
        playable: impl Fn(&PlaylistEntry) -> bool,
    ) -> Option<EntryId> {
        if auto
            && self.repeat == RepeatMode::One
            && let Some(c) = current
            && playlist.get(c).is_some_and(&playable)
        {
            return Some(c);
        }
        let len = self.sequence_len(playlist);
        if len == 0 {
            return None;
        }
        // Start scanning after the current entry, or at the gap a removed entry left.
        let (first, current_pos) = match current.and_then(|c| self.position(playlist, c)) {
            Some(p) => (p + 1, Some(p)),
            None => match self.anchor {
                Some(a) if current.is_some() => (a, None),
                _ => (0, None),
            },
        };
        let wrap = self.repeat == RepeatMode::All;
        let candidates: Vec<usize> = if wrap {
            // Everything after, then from the top, ending with the current entry itself.
            let end = current_pos.map(|p| p + 1).unwrap_or(first);
            (first..len).chain(0..end.min(len)).collect()
        } else {
            (first..len).collect()
        };
        let found = candidates.into_iter().map(|i| self.at(playlist, i)).find(|id| playlist.get(*id).is_some_and(&playable));
        if let Some(id) = found {
            self.anchor = self.position(playlist, id);
        }
        found
    }

    /// The entry before `current`.
    pub fn previous(
        &mut self,
        playlist: &Playlist,
        current: Option<EntryId>,
        playable: impl Fn(&PlaylistEntry) -> bool,
    ) -> Option<EntryId> {
        let len = self.sequence_len(playlist);
        if len == 0 {
            return None;
        }
        let start = match current.and_then(|c| self.position(playlist, c)) {
            Some(p) => p,
            None => self.anchor.unwrap_or(0).min(len),
        };
        let mut candidates: Vec<usize> = (0..start).rev().collect();
        if self.repeat == RepeatMode::All {
            candidates.extend((start + 1..len).rev());
        }
        let found = candidates.into_iter().map(|i| self.at(playlist, i)).find(|id| playlist.get(*id).is_some_and(&playable));
        if let Some(id) = found {
            self.anchor = self.position(playlist, id);
        }
        found
    }

    fn sequence_len(&self, playlist: &Playlist) -> usize {
        if self.shuffle { self.order.len() } else { playlist.len() }
    }

    fn position(&self, playlist: &Playlist, id: EntryId) -> Option<usize> {
        if self.shuffle { self.order.iter().position(|x| *x == id) } else { playlist.index_of(id) }
    }

    fn at(&self, playlist: &Playlist, i: usize) -> EntryId {
        if self.shuffle { self.order[i] } else { playlist.entries()[i].id }
    }

    /// xorshift64*: small, fast, and good enough for shuffling a playlist.
    fn next_random(&mut self) -> u64 {
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::playlist::EntryStatus;

    fn playlist(n: usize) -> Playlist {
        let mut p = Playlist::new("t");
        p.add_paths((0..n).map(|i| PathBuf::from(format!("{i}"))));
        p
    }

    fn all(_: &PlaylistEntry) -> bool {
        true
    }

    fn name(p: &Playlist, id: Option<EntryId>) -> Option<String> {
        id.map(|id| p.get(id).unwrap().path.display().to_string())
    }

    #[test]
    fn sequential_with_repeat_modes() {
        let p = playlist(3);
        let ids: Vec<_> = p.ids().collect();
        let mut nav = Navigator::new(1);
        assert_eq!(nav.next(&p, None, false, all), Some(ids[0]));
        assert_eq!(nav.next(&p, Some(ids[0]), true, all), Some(ids[1]));
        assert_eq!(nav.next(&p, Some(ids[2]), true, all), None);
        nav.repeat = RepeatMode::All;
        assert_eq!(nav.next(&p, Some(ids[2]), true, all), Some(ids[0]));
        assert_eq!(nav.previous(&p, Some(ids[0]), all), Some(ids[2]));
        nav.repeat = RepeatMode::One;
        assert_eq!(nav.next(&p, Some(ids[1]), true, all), Some(ids[1]), "auto repeats");
        assert_eq!(nav.next(&p, Some(ids[1]), false, all), Some(ids[2]), "manual next moves on");
        nav.repeat = RepeatMode::Off;
        assert_eq!(nav.previous(&p, Some(ids[0]), all), None);
    }

    #[test]
    fn repeat_all_with_single_track_replays_it() {
        let p = playlist(1);
        let id = p.entries()[0].id;
        let mut nav = Navigator::new(1);
        nav.repeat = RepeatMode::All;
        assert_eq!(nav.next(&p, Some(id), true, all), Some(id));
    }

    #[test]
    fn skips_unplayable_entries() {
        let mut p = playlist(4);
        let ids: Vec<_> = p.ids().collect();
        p.get_mut(ids[1]).unwrap().status = EntryStatus::Missing;
        p.get_mut(ids[2]).unwrap().status = EntryStatus::Unsupported("x".into());
        let mut nav = Navigator::new(1);
        let ok = |e: &PlaylistEntry| e.status.is_playable();
        assert_eq!(nav.next(&p, Some(ids[0]), true, ok), Some(ids[3]));
        assert_eq!(nav.previous(&p, Some(ids[3]), ok), Some(ids[0]));
        let mut broken = playlist(2);
        for id in broken.ids().collect::<Vec<_>>() {
            broken.get_mut(id).unwrap().status = EntryStatus::Missing;
        }
        nav.repeat = RepeatMode::All;
        assert_eq!(nav.next(&broken, None, true, ok), None, "no infinite loop when nothing is playable");
    }

    #[test]
    fn continues_after_current_entry_is_removed() {
        let mut p = playlist(4);
        let ids: Vec<_> = p.ids().collect();
        let mut nav = Navigator::new(1);
        nav.sync(&p, Some(ids[1]));
        p.remove(&HashSet::from([ids[1]]));
        nav.sync(&p, Some(ids[1]));
        assert_eq!(name(&p, nav.next(&p, Some(ids[1]), true, all)).as_deref(), Some("2"));
        nav.sync(&p, Some(ids[1]));
        assert_eq!(name(&p, nav.previous(&p, Some(ids[1]), all)).as_deref(), Some("0"));
    }

    #[test]
    fn shuffle_plays_every_entry_once_per_cycle() {
        let p = playlist(50);
        let mut nav = Navigator::new(42);
        let first = p.entries()[10].id;
        nav.set_shuffle(true, &p, Some(first));
        let mut seen = HashSet::from([first]);
        let mut cur = Some(first);
        while let Some(n) = nav.next(&p, cur, true, all) {
            assert!(seen.insert(n), "repeated before cycle end");
            cur = Some(n);
        }
        assert_eq!(seen.len(), 50);
        let in_order: Vec<_> = p.ids().collect();
        assert_ne!(nav.order, in_order, "order is actually shuffled");
    }

    #[test]
    fn shuffle_previous_retraces_order() {
        let p = playlist(10);
        let mut nav = Navigator::new(7);
        nav.set_shuffle(true, &p, None);
        let a = nav.next(&p, None, false, all).unwrap();
        let b = nav.next(&p, Some(a), false, all).unwrap();
        let c = nav.next(&p, Some(b), false, all).unwrap();
        assert_eq!(nav.previous(&p, Some(c), all), Some(b));
        assert_eq!(nav.previous(&p, Some(b), all), Some(a));
    }

    #[test]
    fn shuffle_adds_new_entries_after_current() {
        let mut p = playlist(5);
        let mut nav = Navigator::new(3);
        let cur = p.entries()[0].id;
        nav.set_shuffle(true, &p, Some(cur));
        let added = p.add_paths([PathBuf::from("new")]);
        nav.sync(&p, Some(cur));
        let pos_cur = nav.order.iter().position(|x| *x == cur).unwrap();
        let pos_new = nav.order.iter().position(|x| *x == added[0]).unwrap();
        assert!(pos_new > pos_cur);
        assert_eq!(nav.order.len(), 6);
    }

    #[test]
    fn repeat_cycle() {
        assert_eq!(RepeatMode::Off.cycle(), RepeatMode::All);
        assert_eq!(RepeatMode::All.cycle(), RepeatMode::One);
        assert_eq!(RepeatMode::One.cycle(), RepeatMode::Off);
    }
}
