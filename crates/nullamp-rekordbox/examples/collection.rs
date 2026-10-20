//! `cargo run --release -p nullamp-rekordbox --example collection -- <master.db> <key> [<share>]`
//!
//! Opens the rekordbox desktop library and prints a summary, so the key and the mapping can be
//! checked against the real database. The key stays on your command line; it is never stored.
//! `<share>` defaults to the `share` folder beside `master.db`.

use std::path::{Path, PathBuf};

use nullamp_rekordbox::Collection;

fn main() {
    let mut args = std::env::args().skip(1);
    let (db, key) = match (args.next(), args.next()) {
        (Some(db), Some(key)) => (PathBuf::from(db), key),
        _ => {
            eprintln!("usage: collection <master.db> <key> [<share>]");
            std::process::exit(2);
        }
    };
    let share = args.next().map(PathBuf::from).unwrap_or_else(|| db.parent().unwrap_or(Path::new(".")).join("share"));

    let col = match Collection::open(&db, &key, &share) {
        Ok(col) => col,
        Err(e) => {
            eprintln!("cannot open {}: {e}", db.display());
            std::process::exit(1);
        }
    };

    let analysed = col.tracks.iter().filter(|t| t.analysis.as_ref().is_some_and(|p| p.exists())).count();
    println!("{} tracks, {} with analysis files present, {} playlists/folders\n", col.tracks.len(), analysed, col.playlists.len());
    for t in col.tracks.iter().take(10) {
        let bpm = t.bpm.map(|b| format!("{b:.2}")).unwrap_or_else(|| "-".into());
        println!("  {:<28} {:<22} {:>7} bpm  key {:<4} {}", ellipsis(&t.artist, 28), ellipsis(&t.title, 22), bpm, t.key.as_deref().unwrap_or("-"), t.file.display());
    }
    if col.tracks.len() > 10 {
        println!("  … and {} more", col.tracks.len() - 10);
    }
    for p in col.playlists.iter().filter(|p| !p.tracks.is_empty()) {
        println!("\nplaylist {:?}: {} tracks", p.name, p.tracks.len());
    }
}

fn ellipsis(s: &str, max: usize) -> String {
    if s.chars().count() <= max { s.to_owned() } else { format!("{}…", s.chars().take(max - 1).collect::<String>()) }
}
