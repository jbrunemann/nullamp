//! Where the tracks of a DJ mix start, as the setlist panel's "Find tracks" finds them.
//!
//! `cargo run --release -p nullamp-audio --example split -- MIX [--tracks N]`
//!
//! Without `--tracks` the number of tracks is guessed.

use std::path::Path;
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut mix, mut tracks) = (None, None);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--tracks" => tracks = args.next().and_then(|v| v.parse::<usize>().ok()),
            _ => mix = Some(a),
        }
    }
    let Some(mix) = mix else {
        eprintln!("usage: split MIX [--tracks N]");
        std::process::exit(2);
    };
    let started = Instant::now();
    match nullamp_audio::find_track_starts(Path::new(&mix), tracks, &|| false) {
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
