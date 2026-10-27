//! `cargo run --release -p nullamp-rekordbox --example keys -- <master.db> <key>`
//!
//! Prints `key<TAB>path` for every track of the desktop library that has a key, so the keys
//! rekordbox analysed can serve as a reference for another key detector.

use std::path::{Path, PathBuf};

use nullamp_rekordbox::Collection;

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(db), Some(key)) = (args.next().map(PathBuf::from), args.next()) else {
        eprintln!("usage: keys <master.db> <key>");
        std::process::exit(2);
    };
    let share = db.parent().unwrap_or(Path::new(".")).join("share");
    let col = match Collection::open(&db, &key, &share) {
        Ok(col) => col,
        Err(e) => {
            eprintln!("cannot open {}: {e}", db.display());
            std::process::exit(1);
        }
    };
    for t in &col.tracks {
        if let Some(k) = t.key.as_deref().filter(|k| !k.is_empty()) {
            println!("{k}\t{}", t.file.display());
        }
    }
}
