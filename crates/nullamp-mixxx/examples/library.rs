//! Prints what Nullamp reads from a Mixxx library: `cargo run -p nullamp-mixxx --example library -- mixxxdb.sqlite`

fn main() {
    let Some(db) = std::env::args().nth(1) else {
        eprintln!("usage: library MIXXXDB.sqlite");
        return;
    };
    let started = std::time::Instant::now();
    match nullamp_mixxx::Library::open(std::path::Path::new(&db)) {
        Ok(lib) => {
            println!("{} tracks in {:?}", lib.tracks.len(), started.elapsed());
            for l in &lib.lists {
                println!("{:?} {:?}: {} tracks", l.kind, l.name, l.tracks.len());
            }
            for t in lib.tracks.iter().take(5) {
                println!("{t:?}");
            }
        }
        Err(e) => eprintln!("{e}"),
    }
}
