//! DJ mix setlists: the constituent tracks of one continuous audio file.
//!
//! Three separate entities are modelled:
//! - the physical audio file (identified by the caller, not stored here),
//! - the mix as a whole ([`MixInfo`]: DJ, title, event, date),
//! - the constituent tracks ([`SetlistEntry`]).
//!
//! Timing honesty matters: an entry records whether its start time is exact
//! (chapters, cue sheets) or approximate (most online tracklists), and the UI is
//! expected to present approximate boundaries as such.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::ids::EntryId;

/// Where a piece of setlist information came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Provenance {
    /// ID3 CHAP/CTOC frames (or another container's chapter list) in the audio file.
    EmbeddedChapters,
    /// A cue sheet next to the audio file.
    CueSheet { path: String },
    /// Text pasted or imported by the user, optionally copied from a web page.
    Imported { origin_url: Option<String> },
    /// Fetched from an online provider.
    Online { provider: String, url: Option<String> },
    /// Typed in by the user.
    Manual,
}

impl Provenance {
    pub fn label(&self) -> String {
        match self {
            Provenance::EmbeddedChapters => "Embedded chapters".into(),
            Provenance::CueSheet { path } => format!("Cue sheet ({})", file_name(path)),
            Provenance::Imported { origin_url: Some(u) } => format!("Imported from {u}"),
            Provenance::Imported { origin_url: None } => "Imported text".into(),
            Provenance::Online { provider, .. } => provider.clone(),
            Provenance::Manual => "Manual entry".into(),
        }
    }

    pub fn url(&self) -> Option<&str> {
        match self {
            Provenance::Imported { origin_url } => origin_url.as_deref(),
            Provenance::Online { url, .. } => url.as_deref(),
            _ => None,
        }
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// How much a start time can be trusted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Timing {
    /// From a source with frame-level precision (cue sheet, chapters) or set by the user.
    Exact,
    /// An estimate, typically from a community tracklist.
    #[default]
    Approximate,
}

/// Whether an entry's identity (artist/title) has been confirmed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verification {
    /// From an authoritative source, or confirmed by the user.
    Confirmed,
    /// A plausible match that has not been confirmed.
    #[default]
    Suggested,
    /// The track is not identified ("ID - ID").
    Unidentified,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SetlistEntry {
    #[serde(skip, default)]
    pub id: EntryId,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub title: String,
    /// Remix / mix version, e.g. "Extended Mix".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Start time in seconds from the beginning of the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artwork_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    pub provenance: Provenance,
    #[serde(default)]
    pub timing: Timing,
    #[serde(default)]
    pub verification: Verification,
    /// Set when the user changed this entry. Refreshing from a source never
    /// overwrites user-edited entries.
    #[serde(default)]
    pub user_edited: bool,
}

impl SetlistEntry {
    pub fn new(artist: impl Into<String>, title: impl Into<String>, start: Option<f64>, provenance: Provenance) -> Self {
        SetlistEntry {
            id: EntryId::new(),
            artist: artist.into(),
            title: title.into(),
            version: None,
            start,
            end: None,
            release: None,
            label: None,
            artwork_url: None,
            source_url: None,
            provenance,
            timing: Timing::Approximate,
            verification: Verification::Suggested,
            user_edited: false,
        }
    }

    /// "Title (Version)".
    pub fn full_title(&self) -> String {
        let title = if self.title.trim().is_empty() { "Unknown track" } else { self.title.trim() };
        match self.version.as_deref().map(str::trim) {
            Some(v) if !v.is_empty() && !title.contains(v) => format!("{title} ({v})"),
            _ => title.to_owned(),
        }
    }

    pub fn display_artist(&self) -> &str {
        let a = self.artist.trim();
        if a.is_empty() { "Unknown artist" } else { a }
    }

    pub fn is_unidentified(&self) -> bool {
        self.verification == Verification::Unidentified || (is_id(&self.artist) && is_id(&self.title))
    }

    fn identity_key(&self) -> String {
        format!("{}\u{1f}{}", normalize_key(&self.artist), normalize_key(&self.full_title()))
    }
}

fn is_id(s: &str) -> bool {
    let s = s.trim();
    s.is_empty() || s.eq_ignore_ascii_case("id")
}

fn normalize_key(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Information about the mix as a whole, independent of its constituent tracks.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MixInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dj: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

impl MixInfo {
    /// Take values from `other` only where this one has none.
    pub fn fill_missing_from(&mut self, other: MixInfo) {
        fill(&mut self.title, other.title);
        fill(&mut self.dj, other.dj);
        fill(&mut self.event, other.event);
        fill(&mut self.date, other.date);
        fill(&mut self.url, other.url);
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Setlist {
    #[serde(default)]
    pub mix: MixInfo,
    #[serde(default)]
    pub entries: Vec<SetlistEntry>,
}

/// A problem found by [`Setlist::validate`]. Problems are reported, never fatal.
#[derive(Clone, Debug, PartialEq)]
pub enum Issue {
    MissingStart { index: usize },
    NegativeTime { index: usize },
    StartBeyondEnd { index: usize, start: f64, duration: f64 },
    EndBeforeStart { index: usize },
    OutOfOrder { index: usize },
    SameStart { index: usize, other: usize },
    Overlap { index: usize, next: usize },
    Duplicate { index: usize, other: usize },
}

impl Issue {
    pub fn index(&self) -> usize {
        match *self {
            Issue::MissingStart { index }
            | Issue::NegativeTime { index }
            | Issue::StartBeyondEnd { index, .. }
            | Issue::EndBeforeStart { index }
            | Issue::OutOfOrder { index }
            | Issue::SameStart { index, .. }
            | Issue::Overlap { index, .. }
            | Issue::Duplicate { index, .. } => index,
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Issue::MissingStart { index } => format!("#{}: no start time", index + 1),
            Issue::NegativeTime { index } => format!("#{}: negative time", index + 1),
            Issue::StartBeyondEnd { index, start, duration } => format!(
                "#{}: starts at {} but the file is only {} long",
                index + 1,
                crate::time::format_clock(*start),
                crate::time::format_clock(*duration)
            ),
            Issue::EndBeforeStart { index } => format!("#{}: ends before it starts", index + 1),
            Issue::OutOfOrder { index } => format!("#{}: starts before the previous entry", index + 1),
            Issue::SameStart { index, other } => format!("#{} and #{} share a start time", other + 1, index + 1),
            Issue::Overlap { index, next } => format!("#{} ends after #{} starts", index + 1, next + 1),
            Issue::Duplicate { index, other } => format!("#{} repeats #{}", index + 1, other + 1),
        }
    }
}

/// Result of [`Setlist::merge_refresh`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MergeReport {
    pub kept_user_edits: usize,
    pub added: usize,
    pub replaced: usize,
}

impl Setlist {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn index_of(&self, id: EntryId) -> Option<usize> {
        self.entries.iter().position(|e| e.id == id)
    }

    /// Index of the entry playing at `position` seconds.
    ///
    /// The active entry is the one with the latest start time not after `position`.
    /// An explicit end time that has passed yields `None` (a gap). Entries without a
    /// start time never become active.
    pub fn active_index(&self, position: f64) -> Option<usize> {
        let mut best: Option<(usize, f64)> = None;
        for (i, e) in self.entries.iter().enumerate() {
            if let Some(start) = e.start
                && start <= position
                && best.is_none_or(|(_, s)| start >= s)
            {
                best = Some((i, start));
            }
        }
        let (index, _) = best?;
        match self.entries[index].end {
            Some(end) if position >= end => None,
            _ => Some(index),
        }
    }

    /// The span `[start, end)` of entry `index`: its own end, else the next later
    /// start, else the file duration.
    pub fn span(&self, index: usize, duration: Option<f64>) -> Option<(f64, f64)> {
        let entry = self.entries.get(index)?;
        let start = entry.start?;
        let next_start = self
            .entries
            .iter()
            .filter_map(|e| e.start)
            .filter(|&s| s > start)
            .fold(None, |acc: Option<f64>, s| Some(acc.map_or(s, |a| a.min(s))));
        let end = entry.end.or(next_start).or(duration)?;
        Some((start, end.max(start)))
    }

    /// Check timestamps and entries. `duration` is the audio file's length if known.
    pub fn validate(&self, duration: Option<f64>) -> Vec<Issue> {
        let mut issues = Vec::new();
        let mut prev_start: Option<(usize, f64)> = None;
        let mut seen: Vec<(String, usize)> = Vec::new();

        for (i, e) in self.entries.iter().enumerate() {
            match e.start {
                None => issues.push(Issue::MissingStart { index: i }),
                Some(s) if s < 0.0 || !s.is_finite() => issues.push(Issue::NegativeTime { index: i }),
                Some(s) => {
                    if let Some(d) = duration
                        && s > d
                    {
                        issues.push(Issue::StartBeyondEnd { index: i, start: s, duration: d });
                    }
                    if let Some((pi, ps)) = prev_start {
                        if s < ps {
                            issues.push(Issue::OutOfOrder { index: i });
                        } else if s == ps {
                            issues.push(Issue::SameStart { index: i, other: pi });
                        }
                    }
                    prev_start = Some((i, s));
                }
            }
            if let Some(end) = e.end {
                if end < 0.0 {
                    issues.push(Issue::NegativeTime { index: i });
                } else if e.start.is_some_and(|s| end < s) {
                    issues.push(Issue::EndBeforeStart { index: i });
                }
            }
            if !e.is_unidentified() {
                let key = e.identity_key();
                if let Some((_, other)) = seen.iter().find(|(k, _)| *k == key) {
                    issues.push(Issue::Duplicate { index: i, other: *other });
                } else {
                    seen.push((key, i));
                }
            }
        }

        for i in 0..self.entries.len().saturating_sub(1) {
            if let (Some(end), Some(next_start)) = (self.entries[i].end, self.entries[i + 1].start)
                && end > next_start
                && self.entries[i].start.is_some_and(|s| s <= next_start)
            {
                issues.push(Issue::Overlap { index: i, next: i + 1 });
            }
        }
        issues
    }

    /// Stable-sort entries by start time; entries without a start keep their
    /// position relative to the entry before them.
    pub fn sort_by_start(&mut self) {
        let mut last = f64::NEG_INFINITY;
        let mut keyed: Vec<(f64, usize, SetlistEntry)> = std::mem::take(&mut self.entries)
            .into_iter()
            .enumerate()
            .map(|(i, e)| {
                if let Some(s) = e.start {
                    last = s;
                }
                (e.start.unwrap_or(last), i, e)
            })
            .collect();
        keyed.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        self.entries = keyed.into_iter().map(|(_, _, e)| e).collect();
    }

    /// Replace this setlist's content with `incoming` (e.g. a refreshed online
    /// result) without discarding the user's edits.
    ///
    /// - Entries the user edited are kept unchanged.
    /// - Incoming entries that match a kept entry (same artist and title) are dropped.
    /// - All other current entries are replaced by the incoming ones.
    /// - Mix information is filled in only where the current value is empty.
    pub fn merge_refresh(&mut self, incoming: Setlist) -> MergeReport {
        let kept: Vec<SetlistEntry> = self.entries.iter().filter(|e| e.user_edited).cloned().collect();
        let kept_keys: HashSet<String> = kept.iter().map(SetlistEntry::identity_key).collect();
        let replaced = self.entries.len() - kept.len();
        let added_entries: Vec<SetlistEntry> =
            incoming.entries.into_iter().filter(|e| !kept_keys.contains(&e.identity_key())).collect();
        let report = MergeReport { kept_user_edits: kept.len(), added: added_entries.len(), replaced };

        self.entries = kept;
        self.entries.extend(added_entries);
        self.sort_by_start();

        self.mix.fill_missing_from(incoming.mix);
        report
    }
}

fn fill(slot: &mut Option<String>, value: Option<String>) {
    if slot.as_deref().is_none_or(|s| s.trim().is_empty()) {
        *slot = value;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(artist: &str, title: &str, start: Option<f64>) -> SetlistEntry {
        SetlistEntry::new(artist, title, start, Provenance::Manual)
    }

    fn sample() -> Setlist {
        Setlist {
            mix: MixInfo { title: Some("Live".into()), ..Default::default() },
            entries: vec![
                entry("A", "One", Some(0.0)),
                entry("B", "Two", Some(300.0)),
                entry("C", "Three", Some(600.0)),
            ],
        }
    }

    #[test]
    fn active_index_follows_position() {
        let s = sample();
        assert_eq!(s.active_index(0.0), Some(0));
        assert_eq!(s.active_index(299.9), Some(0));
        assert_eq!(s.active_index(300.0), Some(1));
        assert_eq!(s.active_index(10_000.0), Some(2));
    }

    #[test]
    fn active_index_handles_gaps_and_unknowns() {
        let mut s = sample();
        s.entries[0].start = Some(30.0);
        assert_eq!(s.active_index(10.0), None, "before the first entry");
        s.entries[1].end = Some(400.0);
        assert_eq!(s.active_index(450.0), None, "explicit end leaves a gap");
        s.entries.insert(1, entry("X", "No time", None));
        assert_eq!(s.active_index(350.0), Some(2));
    }

    #[test]
    fn active_index_tolerates_unsorted_entries() {
        let mut s = sample();
        s.entries.swap(0, 2);
        assert_eq!(s.active_index(650.0), Some(0));
        assert_eq!(s.active_index(10.0), Some(2));
    }

    #[test]
    fn spans() {
        let s = sample();
        assert_eq!(s.span(0, Some(900.0)), Some((0.0, 300.0)));
        assert_eq!(s.span(2, Some(900.0)), Some((600.0, 900.0)));
        assert_eq!(s.span(2, None), None);
    }

    #[test]
    fn validation_reports_problems() {
        let mut s = sample();
        s.entries.push(entry("D", "Four", Some(500.0)));
        s.entries.push(entry("a", "one", Some(700.0)));
        s.entries.push(entry("E", "Five", None));
        s.entries.push(entry("F", "Six", Some(2000.0)));
        s.entries[1].end = Some(650.0);
        let issues = s.validate(Some(1000.0));
        assert!(issues.contains(&Issue::OutOfOrder { index: 3 }));
        assert!(issues.contains(&Issue::Duplicate { index: 4, other: 0 }));
        assert!(issues.contains(&Issue::MissingStart { index: 5 }));
        assert!(issues.contains(&Issue::StartBeyondEnd { index: 6, start: 2000.0, duration: 1000.0 }));
        assert!(issues.contains(&Issue::Overlap { index: 1, next: 2 }));
        assert!(sample().validate(Some(1000.0)).is_empty());
    }

    #[test]
    fn unidentified_tracks_are_not_duplicates() {
        let s = Setlist {
            mix: MixInfo::default(),
            entries: vec![entry("ID", "ID", Some(0.0)), entry("ID", "ID", Some(60.0))],
        };
        assert!(s.validate(None).is_empty());
    }

    #[test]
    fn same_start_and_end_before_start() {
        let mut s = sample();
        s.entries[2].start = Some(300.0);
        s.entries[0].end = Some(-1.0);
        s.entries[1].end = Some(100.0);
        let issues = s.validate(None);
        assert!(issues.contains(&Issue::SameStart { index: 2, other: 1 }));
        assert!(issues.contains(&Issue::NegativeTime { index: 0 }));
        assert!(issues.contains(&Issue::EndBeforeStart { index: 1 }));
    }

    #[test]
    fn merge_keeps_user_edits() {
        let mut current = sample();
        current.entries[1].title = "Two (my fix)".into();
        current.entries[1].start = Some(310.0);
        current.entries[1].user_edited = true;
        current.mix.dj = None;

        let incoming = Setlist {
            mix: MixInfo { title: Some("Other".into()), dj: Some("DJ".into()), ..Default::default() },
            entries: vec![
                entry("A", "One", Some(0.0)),
                entry("B", "Two (my fix)", Some(290.0)),
                entry("C", "Three", Some(620.0)),
                entry("D", "Four", Some(900.0)),
            ],
        };
        let report = current.merge_refresh(incoming);
        assert_eq!(report, MergeReport { kept_user_edits: 1, added: 3, replaced: 2 });
        let titles: Vec<_> = current.entries.iter().map(|e| (e.title.as_str(), e.start)).collect();
        assert_eq!(
            titles,
            [("One", Some(0.0)), ("Two (my fix)", Some(310.0)), ("Three", Some(620.0)), ("Four", Some(900.0))]
        );
        assert_eq!(current.mix.title.as_deref(), Some("Live"), "existing mix info kept");
        assert_eq!(current.mix.dj.as_deref(), Some("DJ"), "missing mix info filled");
    }

    #[test]
    fn sort_keeps_untimed_entries_near_predecessor() {
        let mut s = Setlist::default();
        s.entries = vec![entry("B", "b", Some(100.0)), entry("X", "x", None), entry("A", "a", Some(0.0))];
        s.sort_by_start();
        let names: Vec<_> = s.entries.iter().map(|e| e.artist.as_str()).collect();
        assert_eq!(names, ["A", "B", "X"]);
    }

    #[test]
    fn full_title_and_serde() {
        let mut e = entry("A", "Song", Some(1.0));
        e.version = Some("Extended Mix".into());
        assert_eq!(e.full_title(), "Song (Extended Mix)");
        e.title = "Song (Extended Mix)".into();
        assert_eq!(e.full_title(), "Song (Extended Mix)");

        let s = Setlist { mix: MixInfo::default(), entries: vec![e] };
        let json = serde_json::to_string(&s).unwrap();
        let back: Setlist = serde_json::from_str(&json).unwrap();
        assert_eq!(back.entries[0].title, s.entries[0].title);
        assert_eq!(back.entries[0].provenance, Provenance::Manual);
    }
}
