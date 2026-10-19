//! ID3v2 chapters (CHAP / CTOC frames), used by DJ mixes and podcasts.

use std::path::Path;

use id3::TagLike;
use nullamp_core::setlist::{MixInfo, Provenance, Setlist, SetlistEntry, Timing, Verification};

#[derive(Clone, Debug, PartialEq)]
pub struct Chapter {
    /// Seconds from the start of the audio.
    pub start: f64,
    pub end: Option<f64>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub url: Option<String>,
}

fn read_tag(path: &Path) -> Result<Option<id3::Tag>, id3::Error> {
    // `read_from_path` detects MP3, WAV and AIFF containers.
    match id3::Tag::read_from_path(path) {
        Ok(tag) => Ok(Some(tag)),
        Err(e) if matches!(e.kind, id3::ErrorKind::NoTag) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Read chapters in playback order. Returns an empty list when there are none.
///
/// If a top-level table of contents exists its order is used; otherwise chapters are
/// sorted by start time.
pub fn read_chapters(path: &Path) -> Result<Vec<Chapter>, id3::Error> {
    let Some(tag) = read_tag(path)? else { return Ok(Vec::new()) };
    let by_id: Vec<(&str, &id3::frame::Chapter)> = tag.chapters().map(|c| (c.element_id.as_str(), c)).collect();
    if by_id.is_empty() {
        return Ok(Vec::new());
    }
    let toc_order: Option<Vec<&str>> = tag
        .tables_of_contents()
        .find(|t| t.top_level)
        .map(|t| t.elements.iter().map(String::as_str).collect());

    let mut ordered: Vec<&id3::frame::Chapter> = match toc_order {
        Some(ids) => ids.iter().filter_map(|id| by_id.iter().find(|(e, _)| e == id).map(|(_, c)| *c)).collect(),
        None => Vec::new(),
    };
    // Chapters not referenced by the TOC are still included.
    for (_, c) in &by_id {
        if !ordered.iter().any(|o| std::ptr::eq(*o, *c)) {
            ordered.push(c);
        }
    }
    if tag.tables_of_contents().find(|t| t.top_level).is_none_or(|t| !t.ordered) {
        ordered.sort_by_key(|c| c.start_time);
    }

    Ok(ordered
        .into_iter()
        .map(|c| {
            let text = |id: &str| {
                c.frames
                    .iter()
                    .find(|f| f.id() == id)
                    .and_then(|f| f.content().text())
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
            };
            let url = c.frames.iter().find_map(|f| match f.content() {
                id3::Content::ExtendedLink(l) => Some(l.link.clone()),
                id3::Content::Link(l) => Some(l.clone()),
                _ => None,
            });
            let end = (c.end_time > c.start_time && c.end_time != u32::MAX).then(|| f64::from(c.end_time) / 1000.0);
            Chapter { start: f64::from(c.start_time) / 1000.0, end, title: text("TIT2"), artist: text("TPE1"), url }
        })
        .collect())
}

/// ID3v2 frames that are not editable [`crate::Field`]s, as (label, text) pairs.
pub(crate) fn other_id3_frames(path: &Path) -> Result<Vec<(String, String)>, id3::Error> {
    const KNOWN: &[&str] = &[
        "TIT2", "TPE1", "TALB", "TDRC", "TYER", "TDAT", "TCON", "TRCK", "COMM", "APIC", "CHAP", "CTOC",
    ];
    let Some(tag) = read_tag(path)? else { return Ok(Vec::new()) };
    Ok(tag
        .frames()
        .filter(|f| !KNOWN.contains(&f.id()))
        .map(|f| {
            let (label, value) = match f.content() {
                id3::Content::ExtendedText(t) => (format!("TXXX:{}", t.description), t.value.clone()),
                id3::Content::ExtendedLink(l) => (format!("WXXX:{}", l.description), l.link.clone()),
                id3::Content::Private(p) => (format!("PRIV:{}", p.owner_identifier), format!("<{} bytes>", p.private_data.len())),
                content => {
                    let text = match content.text() {
                        Some(t) => t.to_owned(),
                        None => match content.link() {
                            Some(l) => l.to_owned(),
                            None => "<binary>".to_owned(),
                        },
                    };
                    (f.id().to_owned(), text)
                }
            };
            let value = value.trim_end_matches('\0').to_owned();
            let value = if value.chars().count() > 500 { format!("{}…", value.chars().take(500).collect::<String>()) } else { value };
            (label, value)
        })
        .collect())
}

/// Turn chapters into a setlist. Chapter titles in the form "Artist - Title" are split.
pub fn chapters_to_setlist(chapters: &[Chapter], mix: MixInfo) -> Setlist {
    let entries = chapters
        .iter()
        .map(|c| {
            let raw_title = c.title.clone().unwrap_or_default();
            let (artist, title) = match (&c.artist, raw_title.split_once(" - ")) {
                (Some(a), _) => (a.clone(), raw_title.clone()),
                (None, Some((a, t))) => (a.trim().to_owned(), t.trim().to_owned()),
                (None, None) => (String::new(), raw_title.clone()),
            };
            let mut e = SetlistEntry::new(artist, title, Some(c.start), Provenance::EmbeddedChapters);
            e.end = c.end;
            e.source_url = c.url.clone();
            e.timing = Timing::Exact;
            e.verification = if e.is_unidentified() { Verification::Unidentified } else { Verification::Suggested };
            e
        })
        .collect();
    Setlist { mix, entries }
}

/// Replace all chapters in an ID3v2 tag with ones built from `setlist`.
/// Entries without a start time are skipped.
pub(crate) fn apply_chapters(tag: &mut id3::Tag, setlist: &Setlist, duration: Option<f64>) {
    tag.remove("CHAP");
    tag.remove("CTOC");
    let mut timed: Vec<&SetlistEntry> = setlist.entries.iter().filter(|e| e.start.is_some()).collect();
    timed.sort_by(|a, b| a.start.unwrap_or(0.0).total_cmp(&b.start.unwrap_or(0.0)));
    let ms = |s: f64| (s.max(0.0) * 1000.0).round().min(f64::from(u32::MAX - 1)) as u32;
    let mut element_ids = Vec::new();
    for (i, e) in timed.iter().enumerate() {
        let start = e.start.unwrap_or(0.0);
        let end = e.end.or_else(|| timed.get(i + 1).and_then(|n| n.start)).or(duration).unwrap_or(start);
        let element_id = format!("chp{i}");
        let mut frames = vec![id3::Frame::text("TIT2", e.full_title())];
        if !e.artist.trim().is_empty() {
            frames.push(id3::Frame::text("TPE1", e.artist.trim()));
        }
        tag.add_frame(id3::frame::Chapter {
            element_id: element_id.clone(),
            start_time: ms(start),
            end_time: ms(end.max(start)),
            start_offset: u32::MAX,
            end_offset: u32::MAX,
            frames,
        });
        element_ids.push(element_id);
    }
    if !element_ids.is_empty() {
        tag.add_frame(id3::frame::TableOfContents {
            element_id: "toc".into(),
            top_level: true,
            ordered: true,
            elements: element_ids,
            frames: Vec::new(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::fixture;

    #[test]
    fn reads_fixture_chapters() {
        let ch = read_chapters(&fixture("mix_chapters.mp3")).unwrap();
        assert_eq!(ch.len(), 3);
        assert_eq!(ch[0].start, 0.0);
        assert_eq!(ch[0].end, Some(2.0));
        assert_eq!(ch[0].title.as_deref(), Some("Opening Track"));
        assert_eq!(ch[0].artist.as_deref(), Some("Artist One"));
        assert_eq!(ch[2].start, 4.0);
    }

    #[test]
    fn no_chapters_is_empty_not_error() {
        assert!(read_chapters(&fixture("tagged_v24.mp3")).unwrap().is_empty());
        assert!(read_chapters(&fixture("tone_mono_22k.wav")).unwrap().is_empty());
    }

    #[test]
    fn to_setlist_splits_artist_title() {
        let chapters = vec![
            Chapter { start: 0.0, end: None, title: Some("A - One".into()), artist: None, url: None },
            Chapter { start: 60.0, end: None, title: Some("Two".into()), artist: Some("B".into()), url: None },
        ];
        let s = chapters_to_setlist(&chapters, MixInfo::default());
        assert_eq!((s.entries[0].artist.as_str(), s.entries[0].title.as_str()), ("A", "One"));
        assert_eq!((s.entries[1].artist.as_str(), s.entries[1].title.as_str()), ("B", "Two"));
        assert!(s.entries.iter().all(|e| e.timing == Timing::Exact));
    }
}
