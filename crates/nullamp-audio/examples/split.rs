//! Where the tracks of a DJ mix start, as the setlist panel's "Find tracks" finds them.
//!
//! `cargo run --release -p nullamp-audio --example split -- MIX [--tracks N] [--from SECONDS] [--to SECONDS]`
//!
//! Without `--tracks` the number of tracks is guessed. With `--from` (and optionally `--to`) only that
//! stretch is searched, as "Find missing times" does between two known times; `--tracks` is then
//! required and counts the track playing at `--from`.

use std::path::Path;
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut mix, mut tracks, mut from, mut to) = (None, None, None, None);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--tracks" => tracks = args.next().and_then(|v| v.parse::<usize>().ok()),
            "--from" => from = args.next().and_then(|v| v.parse::<f64>().ok()),
            "--to" => to = args.next().and_then(|v| v.parse::<f64>().ok()),
            _ => mix = Some(a),
        }
    }
    let Some(mix) = mix else {
        eprintln!("usage: split MIX [--tracks N] [--from SECONDS] [--to SECONDS]");
        std::process::exit(2);
    };
    let started = Instant::now();
    let found = match (from, tracks) {
        (Some(from), Some(tracks)) => nullamp_audio::find_track_starts_between(Path::new(&mix), from, to, tracks, &|| false),
        (Some(_), None) => {
            eprintln!("--from needs --tracks");
            std::process::exit(2);
        }
        _ => nullamp_audio::find_track_starts(Path::new(&mix), tracks, &|| false),
    };
    match found {
        Ok(starts) => {
            println!("{} tracks in {:.1} s", starts.len(), started.elapsed().as_secs_f64());
            for (i, s) in starts.iter().enumerate() {
                println!("  {:>2}  {}", i + 1, clock(*s));
            }
        }
        Err(e) => {
            eprintln!("{mix}: {e}");
            std::process::exit(1);
        }
    }
}

fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0).round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}
