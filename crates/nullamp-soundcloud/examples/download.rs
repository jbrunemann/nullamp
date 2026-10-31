//! Downloads a SoundCloud track or set: `cargo run -p nullamp-soundcloud --example download -- URL [OUT_DIR]`
//!
//! Picks `OUT_DIR` from the argument, else uses the current directory. Writes the client_id cache
//! into a `.nullamp-soundcloud-cache` folder next to that so a second run skips the scrape.

use std::io::Write;
use std::path::PathBuf;

fn main() {
    let mut args = std::env::args().skip(1);
    let url = args.next().expect("USAGE: download URL [OUT_DIR]");
    let out_dir: PathBuf = args.next().map(PathBuf::from).unwrap_or_else(|| std::env::current_dir().unwrap());
    let cache_dir = out_dir.join(".nullamp-soundcloud-cache");

    let mut last_percent: i32 = -1;
    let result = nullamp_soundcloud::download(&url, &out_dir, Some(&cache_dir), |step| match step {
        nullamp_soundcloud::Step::LookingUpClientId => println!("Looking up SoundCloud client_id..."),
        nullamp_soundcloud::Step::Resolving => println!("Resolving track..."),
        nullamp_soundcloud::Step::Streaming { track, segments } => {
            println!("{} - {} ({} ms, {segments} segments)", track.artist, track.title, track.duration_ms);
            last_percent = -1;
        }
        nullamp_soundcloud::Step::Segment { done, total, bytes } => {
            let percent = (done * 100 / total.max(1)) as i32;
            if percent != last_percent {
                last_percent = percent;
                print!("\r  {percent:>3}% ({done}/{total}, {:.1} MB)", bytes as f64 / (1 << 20) as f64);
                let _ = std::io::stdout().flush();
            }
        }
        nullamp_soundcloud::Step::Done { path, bytes } => println!("\nSaved {} ({:.1} MB)", path.display(), bytes as f64 / (1 << 20) as f64),
    });
    match result {
        Ok(_) => {}
        Err(e) => {
            eprintln!("\nError: {e}");
            std::process::exit(1);
        }
    }
}
