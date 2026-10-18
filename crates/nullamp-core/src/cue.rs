//! Cue sheet parsing.
//!
//! Supports the commands that matter for DJ mixes and albums: `FILE`, `TRACK`,
//! `TITLE`, `PERFORMER`, `INDEX`, `ISRC` and the common `REM` comments. Malformed
//! lines are skipped and reported rather than aborting the whole sheet.

use crate::setlist::{MixInfo, Provenance, Setlist, SetlistEntry, Timing, Verification};
use crate::text::decode_lossy;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CueSheet {
    pub title: Option<String>,
    pub performer: Option<String>,
    pub date: Option<String>,
    pub genre: Option<String>,
    pub files: Vec<CueFile>,
    /// Line numbers and text of lines that could not be understood.
    pub warnings: Vec<(usize, String)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CueFile {
    pub name: String,
    pub tracks: Vec<CueTrack>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CueTrack {
    pub number: u32,
    pub title: Option<String>,
    pub performer: Option<String>,
    pub isrc: Option<String>,
    /// `INDEX 01` in seconds: where the track proper begins.
    pub start: Option<f64>,
    /// `INDEX 00` (pregap start) in seconds.
    pub pregap: Option<f64>,
}

/// Cue sheets are small; anything bigger than this is not a cue sheet.
pub const MAX_CUE_BYTES: usize = 1024 * 1024;

pub fn parse(bytes: &[u8]) -> CueSheet {
    let bytes = &bytes[..bytes.len().min(MAX_CUE_BYTES)];
    let text = decode_lossy(bytes);
    let mut sheet = CueSheet::default();
    let mut current_track: Option<CueTrack> = None;

    fn flush(sheet: &mut CueSheet, track: &mut Option<CueTrack>) {
        if let Some(t) = track.take() {
            if sheet.files.is_empty() {
                sheet.files.push(CueFile::default());
            }
            sheet.files.last_mut().unwrap().tracks.push(t);
        }
    }

    for (idx, raw) in text.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let (command, rest) = split_word(line);
        match command.to_ascii_uppercase().as_str() {
            "FILE" => {
                // rekordbox recordings name each track's source file inside the TRACK
                // block, before its INDEX. That is metadata, not a new section. A FILE
                // after INDEX 00 is the EAC gaps-appended layout and does start one.
                if current_track.as_ref().is_some_and(|t| t.start.is_none() && t.pregap.is_none()) {
                    continue;
                }
                flush(&mut sheet, &mut current_track);
                let (name, _file_type) = take_string(rest);
                sheet.files.push(CueFile { name, tracks: Vec::new() });
            }
            "TRACK" => {
                flush(&mut sheet, &mut current_track);
                let (num, kind) = split_word(rest);
                match num.parse::<u32>() {
                    Ok(number) if kind.trim().eq_ignore_ascii_case("AUDIO") || kind.trim().is_empty() => {
                        current_track = Some(CueTrack { number, ..Default::default() });
                    }
                    Ok(_) => {} // Data tracks are ignored.
                    Err(_) => sheet.warnings.push((line_no, line.to_owned())),
                }
            }
            "TITLE" => {
                let value = Some(take_string(rest).0).filter(|s| !s.is_empty());
                match current_track.as_mut() {
                    Some(t) => t.title = value,
                    None => sheet.title = value,
                }
            }
            "PERFORMER" => {
                let value = Some(take_string(rest).0).filter(|s| !s.is_empty());
                match current_track.as_mut() {
                    Some(t) => t.performer = value,
                    None => sheet.performer = value,
                }
            }
            "ISRC" => {
                if let Some(t) = current_track.as_mut() {
                    t.isrc = Some(rest.trim().to_owned());
                }
            }
            "INDEX" => {
                let (num, time) = split_word(rest);
                match (num.parse::<u32>(), parse_msf(time.trim()), current_track.as_mut()) {
                    (Ok(1), Some(secs), Some(t)) => t.start = Some(secs),
                    (Ok(0), Some(secs), Some(t)) => t.pregap = Some(secs),
                    (Ok(_), Some(_), Some(_)) => {}
                    _ => sheet.warnings.push((line_no, line.to_owned())),
                }
            }
            "REM" => {
                let (key, value) = split_word(rest);
                let value = Some(take_string(value).0).filter(|s| !s.is_empty());
                if current_track.is_none() {
                    match key.to_ascii_uppercase().as_str() {
                        "DATE" => sheet.date = value,
                        "GENRE" => sheet.genre = value,
                        _ => {}
                    }
                }
            }
            "CATALOG" | "CDTEXTFILE" | "FLAGS" | "PREGAP" | "POSTGAP" | "SONGWRITER" => {}
            _ => sheet.warnings.push((line_no, line.to_owned())),
        }
    }
    flush(&mut sheet, &mut current_track);
    sheet
}

fn split_word(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    match s.find(char::is_whitespace) {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    }
}

/// Read a possibly quoted string; return it and the remainder of the line.
fn take_string(s: &str) -> (String, &str) {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix('"') {
        match rest.find('"') {
            Some(end) => (rest[..end].to_owned(), rest[end + 1..].trim()),
            None => (rest.to_owned(), ""),
        }
    } else if s.contains('"') {
        (s.trim_matches('"').to_owned(), "")
    } else {
        // Unquoted: for FILE lines the last word is the file type.
        match s.rsplit_once(char::is_whitespace) {
            Some((name, kind)) if is_file_type(kind) => (name.trim().to_owned(), kind),
            _ => (s.to_owned(), ""),
        }
    }
}

fn is_file_type(s: &str) -> bool {
    matches!(s.to_ascii_uppercase().as_str(), "WAVE" | "MP3" | "AIFF" | "BINARY" | "MOTOROLA" | "FLAC")
}

/// `mm:ss:ff` where `ff` is 1/75 s frames. Minutes may exceed 99.
pub fn parse_msf(s: &str) -> Option<f64> {
    let mut parts = s.split(':');
    let m: u64 = parts.next()?.parse().ok()?;
    let sec: u64 = parts.next()?.parse().ok()?;
    let f: u64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || sec >= 60 || f >= 75 {
        return None;
    }
    Some((m * 60 + sec) as f64 + f as f64 / 75.0)
}

pub fn format_msf(secs: f64) -> String {
    let frames_total = (secs.max(0.0) * 75.0).round() as u64;
    let (m, s, f) = (frames_total / (75 * 60), (frames_total / 75) % 60, frames_total % 75);
    format!("{m:02}:{s:02}:{f:02}")
}

impl CueSheet {
    /// The file section that describes `audio_file_name`, falling back to the only
    /// section when there is exactly one.
    pub fn file_for(&self, audio_file_name: &str) -> Option<&CueFile> {
        let wanted = audio_file_name.to_lowercase();
        let stem = |n: &str| n.rsplit(['/', '\\']).next().unwrap_or(n).rsplit_once('.').map(|(s, _)| s.to_owned()).unwrap_or_else(|| n.to_owned());
        self.files
            .iter()
            .find(|f| f.name.rsplit(['/', '\\']).next().unwrap_or(&f.name).to_lowercase() == wanted)
            // Cue sheets often reference a WAV that was later encoded to MP3/FLAC.
            .or_else(|| self.files.iter().find(|f| stem(&f.name).to_lowercase() == stem(&wanted)))
            .or_else(|| if self.files.len() == 1 { self.files.first() } else { None })
    }

    /// Build a setlist for `audio_file_name` from this sheet.
    pub fn to_setlist(&self, audio_file_name: &str, cue_path: &str) -> Option<Setlist> {
        let file = self.file_for(audio_file_name)?;
        if file.tracks.is_empty() {
            return None;
        }
        let provenance = Provenance::CueSheet { path: cue_path.to_owned() };
        let entries = file
            .tracks
            .iter()
            .map(|t| {
                let mut e = SetlistEntry::new(
                    t.performer.clone().or_else(|| self.performer.clone()).unwrap_or_default(),
                    t.title.clone().unwrap_or_default(),
                    t.start,
                    provenance.clone(),
                );
                e.timing = Timing::Exact;
                e.verification = if e.artist.is_empty() && e.title.is_empty() { Verification::Unidentified } else { Verification::Suggested };
                e
            })
            .collect();
        Some(Setlist {
            mix: MixInfo { title: self.title.clone(), dj: self.performer.clone(), date: self.date.clone(), ..Default::default() },
            entries,
        })
    }
}

/// Write a single-file cue sheet for a setlist.
pub fn write(setlist: &Setlist, audio_file_name: &str) -> String {
    let q = |s: &str| s.replace('"', "'");
    let mut out = String::new();
    if let Some(dj) = &setlist.mix.dj {
        out.push_str(&format!("PERFORMER \"{}\"\n", q(dj)));
    }
    if let Some(title) = &setlist.mix.title {
        out.push_str(&format!("TITLE \"{}\"\n", q(title)));
    }
    if let Some(date) = &setlist.mix.date {
        out.push_str(&format!("REM DATE \"{}\"\n", q(date)));
    }
    let kind = if audio_file_name.to_lowercase().ends_with(".wav") { "WAVE" } else { "MP3" };
    out.push_str(&format!("FILE \"{}\" {kind}\n", q(audio_file_name)));
    for (n, e) in setlist.entries.iter().filter(|e| e.start.is_some()).enumerate() {
        out.push_str(&format!("  TRACK {:02} AUDIO\n", n + 1));
        out.push_str(&format!("    TITLE \"{}\"\n", q(&e.full_title())));
        out.push_str(&format!("    PERFORMER \"{}\"\n", q(&e.artist)));
        out.push_str(&format!("    INDEX 01 {}\n", format_msf(e.start.unwrap_or(0.0))));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIX: &str = r#"REM GENRE "Trance"
REM DATE 2019
PERFORMER "DJ Example"
TITLE "Live at Somewhere"
FILE "mix.wav" WAVE
  TRACK 01 AUDIO
    TITLE "Opener"
    PERFORMER "Artist One"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "Second (Club Mix)"
    PERFORMER "Artist Two"
    INDEX 00 04:58:00
    INDEX 01 05:00:37
  TRACK 03 AUDIO
    TITLE "Long One"
    INDEX 01 125:30:74
"#;

    #[test]
    fn parses_mix_cue() {
        let s = parse(MIX.as_bytes());
        assert_eq!(s.title.as_deref(), Some("Live at Somewhere"));
        assert_eq!(s.performer.as_deref(), Some("DJ Example"));
        assert_eq!(s.date.as_deref(), Some("2019"));
        assert_eq!(s.genre.as_deref(), Some("Trance"));
        assert_eq!(s.files.len(), 1);
        let t = &s.files[0].tracks;
        assert_eq!(t.len(), 3);
        assert_eq!(t[1].start, Some(300.0 + 37.0 / 75.0));
        assert_eq!(t[1].pregap, Some(298.0));
        assert_eq!(t[2].start, Some(125.0 * 60.0 + 30.0 + 74.0 / 75.0));
        assert!(s.warnings.is_empty(), "{:?}", s.warnings);
    }

    #[test]
    fn setlist_matches_encoded_file_by_stem() {
        let s = parse(MIX.as_bytes());
        let list = s.to_setlist("MIX.mp3", "/m/mix.cue").unwrap();
        assert_eq!(list.entries.len(), 3);
        assert_eq!(list.entries[0].artist, "Artist One");
        assert_eq!(list.entries[2].artist, "DJ Example", "falls back to sheet performer");
        assert!(list.entries.iter().all(|e| e.timing == Timing::Exact));
        assert_eq!(list.mix.dj.as_deref(), Some("DJ Example"));
    }

    #[test]
    fn multi_file_sheets_pick_the_right_file() {
        let text = "FILE \"a.flac\" WAVE\n TRACK 01 AUDIO\n TITLE \"A\"\n INDEX 01 00:00:00\nFILE \"b.flac\" WAVE\n TRACK 02 AUDIO\n TITLE \"B\"\n INDEX 01 00:00:00\n";
        let s = parse(text.as_bytes());
        assert_eq!(s.file_for("b.flac").unwrap().tracks[0].title.as_deref(), Some("B"));
        assert!(s.file_for("c.flac").is_none());
    }

    #[test]
    fn rekordbox_per_track_files_stay_in_the_recording() {
        let text = "REM DATE 2026-10-18 12:50 PM\nREM RECORDED_BY \"rekordbox-dj\"\nTITLE \"REC-2026-10-18\"\nPERFORMER \"Mlbru\"\nFILE \"01 REC-2026-10-18.wav\" WAVE\n\tTRACK 01 AUDIO\n\t\tTITLE \"Ill Ghetto Funk\"\n\t\tPERFORMER \"Kolter\"\n\t\tFILE \"/Users/x/Kolter - Ill Ghetto Funk.mp3\" WAVE\n\t\tINDEX 01 00:00:00\n\tTRACK 02 AUDIO\n\t\tTITLE \"Deep Innocence (PIVLIM002)\"\n\t\tPERFORMER \"A2 Chris Stussy\"\n\t\tFILE \"/Users/x/A2 Chris Stussy - Deep Innocence (PIVLIM002).mp3\" WAVE\n\t\tINDEX 01 00:00:19\n";
        let s = parse(text.as_bytes());
        assert!(s.warnings.is_empty(), "{:?}", s.warnings);
        assert_eq!(s.files.len(), 1);
        let list = s.to_setlist("01 REC-2026-10-18.wav", "x.cue").unwrap();
        assert_eq!(list.entries.len(), 2);
        assert_eq!(list.entries[1].artist, "A2 Chris Stussy");
        assert_eq!(list.entries[1].start, Some(19.0 / 75.0));
    }

    #[test]
    fn file_after_pregap_starts_a_new_section() {
        let text = "FILE \"a.wav\" WAVE\nTRACK 01 AUDIO\nINDEX 01 00:00:00\nTRACK 02 AUDIO\nINDEX 00 03:00:00\nFILE \"b.wav\" WAVE\nINDEX 01 00:00:00\n";
        let s = parse(text.as_bytes());
        assert_eq!(s.files.len(), 2);
        assert_eq!(s.files[0].tracks.len(), 2);
    }

    #[test]
    fn tolerates_malformed_input() {
        let text = "\u{FEFF}FILE mix.mp3 MP3\r\nTRACK xx AUDIO\r\nTRACK 01 AUDIO\r\nINDEX 01 00:99:00\r\nINDEX 01 01:00:00\r\nGARBAGE line\r\nTITLE unquoted title words\r\n";
        let s = parse(text.as_bytes());
        assert_eq!(s.files[0].name, "mix.mp3");
        let t = &s.files[0].tracks[0];
        assert_eq!(t.start, Some(60.0));
        assert_eq!(t.title.as_deref(), Some("unquoted title words"));
        assert_eq!(s.warnings.len(), 3);
    }

    #[test]
    fn msf_round_trip_and_bounds() {
        assert_eq!(parse_msf("01:02:03"), Some(62.0 + 3.0 / 75.0));
        assert_eq!(parse_msf("00:60:00"), None);
        assert_eq!(parse_msf("00:00:75"), None);
        assert_eq!(parse_msf("1:2"), None);
        assert_eq!(format_msf(62.04), "01:02:03");
    }

    #[test]
    fn write_then_parse() {
        let s = parse(MIX.as_bytes());
        let list = s.to_setlist("mix.wav", "mix.cue").unwrap();
        let text = write(&list, "mix.mp3");
        let back = parse(text.as_bytes()).to_setlist("mix.mp3", "x").unwrap();
        assert_eq!(back.entries.len(), 3);
        for (a, b) in list.entries.iter().zip(&back.entries) {
            assert_eq!(a.title, b.title);
            assert!((a.start.unwrap() - b.start.unwrap()).abs() < 1.0 / 75.0);
        }
    }

    #[test]
    fn huge_input_is_truncated_not_fatal() {
        let big = "REM x\n".repeat(400_000);
        let s = parse(big.as_bytes());
        assert!(s.files.is_empty());
    }
}
