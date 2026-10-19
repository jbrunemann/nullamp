//! Times the tempo pass: `cargo run --release -p nullamp-audio --example tempo FILE...`

use std::time::Instant;

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    assert!(!files.is_empty(), "usage: tempo FILE...");
    for path in files {
        let start = Instant::now();
        let result = nullamp_audio::compute_tempo(path.as_ref(), &|| false);
        let took = start.elapsed();
        match result {
            Ok(Some(t)) => println!("{:>6.1} BPM  confidence {:.2}  {took:.2?}  {path}", t.bpm, t.confidence),
            Ok(None) => println!("no clear beat  {took:.2?}  {path}"),
            Err(e) => println!("{e}  {path}"),
        }
    }
}
