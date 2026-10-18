//! Parsing of tracklists pasted as plain text.
//!
//! This is how users bring in tracklists from sites that offer no API (for example
//! 1001Tracklists): copy the list from the page, paste it, and keep the page URL as
//! the source. Common shapes are handled:
//!
//! ```text
//! [00:00] Artist - Title (Remix) [LABEL]
//! 01. Artist - Title
//! 1:02:03 Artist – Title
//! 03 [12:30] Artist ft. Other - Title
//! w/ Artist - Title          (played together with the previous track)
//! ID - ID
//! ```

use std::sync::LazyLock;

use regex::Regex;

use crate::setlist::{Provenance, Setlist, SetlistEntry, Timing, Verification};
use crate::time::parse_timestamp;

static NUMBERING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:\d{1,3}\s*[-–:.)]|#\d{1,3}|0\d|\d{1,3}\s+[\[(]\d)\s*").unwrap());
static BRACKET_TIME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\[(](\d{1,3}(?::\d{1,2}){1,2}(?:\.\d+)?)[\])]\s*").unwrap());
static BARE_TIME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\d{1,3}(?::\d{1,2}){1,2}(?:\.\d+)?)(?:\s*[-–|.]\s*|\s+)").unwrap());
static TRAILING_LABEL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s*\[([^\[\]]+)\]\s*$").unwrap());
static VERSION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\s*\(([^()]*\b(?:remix|mix|edit|dub|bootleg|rework|vip|version|remaster(?:ed)?|instrumental|re-?edit|flip|cut)\b[^()]*)\)\s*$").unwrap()
});

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParsedTracklist {
    pub setlist: Setlist,
    /// Lines that did not look like tracks (headers, notes).
    pub ignored: Vec<String>,
}

pub fn parse(text: &str, origin_url: Option<String>) -> ParsedTracklist {
    let provenance = Provenance::Imported { origin_url };
    let mut out = ParsedTracklist::default();

    for raw in text.lines() {
        let line = raw.trim().trim_start_matches('\u{FEFF}');
        if line.is_empty() {
            continue;
        }
        let (together, line) = match line.strip_prefix("w/").or_else(|| line.strip_prefix("W/")) {
            Some(rest) => (true, rest.trim()),
            None => (false, line),
        };
        let (start, rest) = take_time_and_numbering(line);
        let Some(mut entry) = parse_track(rest, start, &provenance) else {
            out.ignored.push(raw.trim().to_owned());
            continue;
        };
        if together {
            // Played simultaneously with the previous entry: share its start.
            entry.start = out.setlist.entries.last().and_then(|e| e.start).or(entry.start);
        }
        out.setlist.entries.push(entry);
    }
    out
}

fn take_time_and_numbering(line: &str) -> (Option<f64>, &str) {
    let mut rest = line;
    let mut start = None;
    // Up to two passes: numbering and time can appear in either order.
    for _ in 0..2 {
        if start.is_none() {
            if let Some(c) = BRACKET_TIME.captures(rest) {
                start = parse_timestamp(&c[1]);
                rest = &rest[c.get(0).unwrap().end()..];
                continue;
            }
            if let Some(c) = BARE_TIME.captures(rest) {
                // Avoid reading "01. Artist" as a timestamp: require a colon.
                if c[1].contains(':') {
                    start = parse_timestamp(&c[1]);
                    rest = &rest[c.get(0).unwrap().end()..];
                    continue;
                }
            }
        }
        if let Some(m) = NUMBERING.find(rest) {
            // The bracketed-time alternative matches into the bracket; keep the bracket.
            let end = rest[..m.end()].rfind(['[', '(']).unwrap_or(m.end());
            rest = rest[end..].trim_start();
            continue;
        }
        break;
    }
    (start, rest.trim())
}

fn parse_track(text: &str, start: Option<f64>, provenance: &Provenance) -> Option<SetlistEntry> {
    let (artist, title) = split_artist_title(text)?;
    let mut title = title.to_owned();
    let mut label = None;
    if let Some(c) = TRAILING_LABEL.captures(&title) {
        label = Some(c[1].trim().to_owned());
        title.truncate(c.get(0).unwrap().start());
    }
    let mut version = None;
    if let Some(c) = VERSION.captures(&title) {
        version = Some(c[1].trim().to_owned());
        title.truncate(c.get(0).unwrap().start());
    }
    let mut e = SetlistEntry::new(artist.trim(), title.trim(), start, provenance.clone());
    e.version = version;
    e.label = label;
    e.timing = Timing::Approximate;
    e.verification = if e.is_unidentified() { Verification::Unidentified } else { Verification::Suggested };
    Some(e)
}

fn split_artist_title(text: &str) -> Option<(&str, &str)> {
    for sep in [" - ", " – ", " — ", " -- "] {
        if let Some((a, t)) = text.split_once(sep)
            && !a.trim().is_empty()
            && !t.trim().is_empty()
        {
            return Some((a, t));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_formats() {
        let text = "Tracklist:\n\
            [00:00] Above & Beyond - Sun & Moon (Club Mix) [ANJUNABEATS]\n\
            02. [04:30] Eric Prydz – Opus\n\
            1:02:03 Deadmau5 - Strobe (Radio Edit)\n\
            w/ Someone - Acapella\n\
            ID - ID\n\
            04 - Artist ft. Guest - Song (Live)\n\
            Some random note\n";
        let p = parse(text, Some("https://example.com/list".into()));
        let e = &p.setlist.entries;
        assert_eq!(e.len(), 6, "{:?}", p.ignored);
        assert_eq!(p.ignored, ["Tracklist:", "Some random note"]);

        assert_eq!(e[0].artist, "Above & Beyond");
        assert_eq!(e[0].title, "Sun & Moon");
        assert_eq!(e[0].version.as_deref(), Some("Club Mix"));
        assert_eq!(e[0].label.as_deref(), Some("ANJUNABEATS"));
        assert_eq!(e[0].start, Some(0.0));

        assert_eq!(e[1].artist, "Eric Prydz");
        assert_eq!(e[1].title, "Opus");
        assert_eq!(e[1].start, Some(270.0));

        assert_eq!(e[2].start, Some(3723.0));
        assert_eq!(e[2].version.as_deref(), Some("Radio Edit"));

        assert_eq!(e[3].start, Some(3723.0), "w/ shares the previous start");
        assert_eq!(e[4].verification, Verification::Unidentified);
        assert_eq!(e[5].artist, "Artist ft. Guest");
        assert_eq!(e[5].title, "Song (Live)", "non-version parentheses stay in the title");
        assert!(e.iter().all(|x| x.timing == Timing::Approximate));
        assert_eq!(e[0].provenance.url(), Some("https://example.com/list"));
    }

    #[test]
    fn numbering_is_not_a_timestamp() {
        let p = parse("01. Artist - Title\n12) Artist Two - Title Two\n808 State - Pacific\n13 [1:00] X - Y\n", None);
        assert_eq!(p.setlist.entries[0].start, None);
        assert_eq!(p.setlist.entries[0].artist, "Artist");
        assert_eq!(p.setlist.entries[1].artist, "Artist Two");
        assert_eq!(p.setlist.entries[2].artist, "808 State");
        assert_eq!(p.setlist.entries[3].start, Some(60.0));
        assert_eq!(p.setlist.entries[3].artist, "X");
    }

    #[test]
    fn time_after_number_and_hyphenated_names() {
        let p = parse("3. 12:30 - Jay-Z - Run-This-Town\n", None);
        let e = &p.setlist.entries[0];
        assert_eq!(e.start, Some(750.0));
        assert_eq!(e.artist, "Jay-Z");
        assert_eq!(e.title, "Run-This-Town");
    }
}
