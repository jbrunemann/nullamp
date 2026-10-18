use std::sync::atomic::{AtomicU64, Ordering};

/// Process-unique identifier for list items (playlist entries, setlist entries).
///
/// Ids are not persisted. They exist so that selection, the current track and
/// in-flight background requests keep pointing at the same item while lists are
/// reordered or edited.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EntryId(u64);

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

impl EntryId {
    pub fn new() -> Self {
        EntryId(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }

    pub fn raw(self) -> u64 {
        self.0
    }
}

impl Default for EntryId {
    fn default() -> Self {
        Self::new()
    }
}
