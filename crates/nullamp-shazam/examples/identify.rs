//! Name the tracks of a mix with Shazam, taking the track starts from a cue sheet, and compare
//! the answers with the cue's titles.
//!
//! `cargo run --release -p nullamp-shazam --example identify -- MIX CUE [--clips N]`
//!
//! Sends N (default 5) requests per track, [`nullamp_shazam::PAUSE`] apart.

use std::path::Path;
use std::time::Instant;

use nullamp_shazam::{CLIP_SECONDS, Error, RATE, Song, Verdict};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: identify MIX CUE [--clips N]");
        std::process::exit(2);
    }
    let clips = args.iter().position(|a| a == "--clips").and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(nullamp_shazam::CLIPS_PER_TRACK).max(1);
    let started = Instant::now();
    let Some(tracks) = std::fs::read(&args[1]).ok().map(|b| nullamp_core::cue::parse(&b)) else {
        eprintln!("cannot read {}", args[1]);
        std::process::exit(1);
    };
    let starts: Vec<(f64, String)> = tracks.files.iter().flat_map(|f| &f.tracks).filter_map(|t| Some((t.start?, t.title.clone().unwrap_or_default()))).collect();
    if starts.is_empty() {
        eprintln!("no track starts in {}", args[1]);
        std::process::exit(1);
    }

    println!("\n  track  clip at  answer");
    let (mut same, mut other_version, mut unsure) = (0, 0, 0);
    let mut first = true;
    for (n, (start, cue_title)) in starts.iter().enumerate() {
        // The last track ends with the mix; seeking past the end only cuts its last clips short.
        let end = starts.get(n + 1).map_or(start + 600.0, |s| s.0);
        let at = nullamp_shazam::clip_starts(*start, end, clips);
        let audio = match nullamp_audio::decode_clips(Path::new(&args[0]), RATE, &at, CLIP_SECONDS, &|| false) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("cannot decode {}: {e}", args[0]);
                std::process::exit(1);
            }
        };
        let mut answers = Vec::new();
        for (clip_at, clip) in at.iter().zip(&audio) {
            if !first {
                std::thread::sleep(nullamp_shazam::PAUSE);
            }
            first = false;
            let line = match nullamp_shazam::recognise(clip) {
                Ok(Some(song)) => {
                    let line = describe(&song);
                    answers.push((*clip_at, song));
                    line
                }
                Ok(None) => "no match".into(),
                Err(Error::TooMany) => {
                    println!("  refused (429); stopping, try again later");
                    std::process::exit(1);
                }
                Err(e) => format!("error: {e}"),
            };
            println!("  {:>5}  {:>7}  {line}", n + 1, clock(*clip_at));
        }
        let (answer, mark) = match nullamp_shazam::agree(&answers) {
            Verdict::Named { song, support } => {
                let mark = match compare(cue_title, &song.title) {
                    Agreement::Same => {
                        same += 1;
                        "OK"
                    }
                    Agreement::OtherVersion => {
                        other_version += 1;
                        "OK, other version"
                    }
                    Agreement::Different => "--",
                };
                (format!("{} - {} ({support} of {clips} clips agree)", song.artist, song.title), mark)
            }
            Verdict::Unsure { candidates } => {
                unsure += 1;
                let list: Vec<String> = candidates.iter().map(|s| format!("{} - {}", s.artist, s.title)).collect();
                (format!("unsure: {}", if list.is_empty() { "no matches".into() } else { list.join(" | ") }), "?")
            }
        };
        println!("  {:>5}  cue      {cue_title}  ->  {answer}  {mark}\n", n + 1);
    }
    let n = starts.len();
    println!("{n} tracks: {same} named like the cue sheet, {other_version} as another version, {unsure} unsure, {} wrong; {:.1} s", n - same - other_version - unsure, started.elapsed().as_secs_f64());
}

enum Agreement {
    Same,
    OtherVersion,
    Different,
}

/// Whether a catalogue title names the cue's song and version. Featured artists and an
/// "original mix" are not versions; cue sheets and catalogues add or leave them out freely.
fn compare(cue_title: &str, catalogue_title: &str) -> Agreement {
    let (cue_song, cue_version) = split_title(cue_title);
    let (song, version) = split_title(catalogue_title);
    if cue_song.is_empty() || cue_song != song {
        Agreement::Different
    } else if cue_version == version {
        Agreement::Same
    } else {
        Agreement::OtherVersion
    }
}

/// The song name and the version of a title like "Song (feat. X) [Y Remix]" or "Song - Y Remix",
/// both reduced to lowercase letters and digits.
fn split_title(title: &str) -> (String, String) {
    let simplify = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).collect::<String>();
    let lower = title.to_lowercase();
    let (mut outside, mut groups, mut current, mut depth) = (String::new(), Vec::new(), String::new(), 0);
    for c in lower.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    groups.push(std::mem::take(&mut current));
                }
            }
            _ if depth > 0 => current.push(c),
            _ => outside.push(c),
        }
    }
    let (song, suffix) = outside.split_once(" - ").unwrap_or((&outside, ""));
    let song = [" feat.", " feat ", " ft.", " ft "].iter().fold(song, |s, cut| s.split(cut).next().unwrap_or(s));
    groups.push(suffix.to_owned());
    let version: Vec<String> = groups
        .iter()
        .map(|g| g.trim())
        .filter(|g| !g.starts_with("feat") && !g.starts_with("ft.") && !g.starts_with("with "))
        .map(simplify)
        .filter(|g| !g.is_empty() && g != "originalmix" && g != "original")
        .collect();
    (simplify(song), version.join(" "))
}

fn describe(song: &Song) -> String {
    format!("{} - {} (at {} in the song, tempo {:+.1}%, pitch {:+.1}%)", song.artist, song.title, clock(song.offset), song.time_skew * 100.0, song.frequency_skew * 100.0)
}

fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0).round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}
