//! Times the tempo pass: `cargo run --release -p nullamp-audio --example tempo FILE[@START-END]...`
//! (START and END in seconds analyse just that part, as for one track of a mix).

use std::time::Instant;

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    assert!(!files.is_empty(), "usage: tempo FILE[@START-END]...");
    for arg in files {
        let (path, span) = match arg.rsplit_once('@').and_then(|(p, s)| Some((p, s.split_once('-')?))) {
            Some((p, (from, to))) => (p.to_owned(), from.parse().ok().zip(to.parse().ok())),
            None => (arg.clone(), None),
        };
        let start = Instant::now();
        let result = nullamp_audio::compute_tempo(path.as_ref(), span, &|| false);
        let took = start.elapsed();
        match result {
            Ok(Some(t)) => println!("{:>6.1} BPM  confidence {:.2}  {took:.2?}  {path}", t.bpm, t.confidence),
            Ok(None) => println!("no clear beat  {took:.2?}  {path}"),
            Err(e) => println!("{e}  {path}"),
        }
    }
}
