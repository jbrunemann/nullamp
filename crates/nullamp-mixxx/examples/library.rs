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
            let started = std::time::Instant::now();
            let waves: Vec<_> = lib.tracks.iter().filter_map(|t| t.waveform().ok().flatten()).collect();
            let tracks_with_analysis = lib.tracks.iter().filter(|t| t.analysis.is_some()).count();
            println!("{} of {} tracks with a waveform file ({} read) in {:?}", tracks_with_analysis, lib.tracks.len(), waves.len(), started.elapsed());
            if let Some(w) = waves.first() {
                let (peak, rms) = w.buckets(2048);
                println!("first: {} samples, bands {}, {} buckets, peak max {:?}, rms mean {:.1}", w.heights.len(), w.bands.is_some(), peak.len(), peak.iter().max(), rms.iter().map(|&r| f32::from(r)).sum::<f32>() / rms.len() as f32);
                println!("colours {:?}", w.bucket_colors(2048).map(|c| c[c.len() / 2]));
            }
        }
        Err(e) => eprintln!("{e}"),
    }
}
