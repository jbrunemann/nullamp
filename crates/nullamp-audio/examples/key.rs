//! Finds the key of files: `cargo run --release -p nullamp-audio --example key [--qm] FILE[@START-END]...`
//! (`--qm` runs the Queen Mary detector Mixxx uses instead).
//! (START and END in seconds analyse just that part, as for one track of a mix).

use std::time::Instant;

const NAMES: [&str; 12] = ["C", "Db", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B"];

fn main() {
    let mut files: Vec<String> = std::env::args().skip(1).collect();
    let qm = files.first().is_some_and(|f| f == "--qm");
    if qm {
        files.remove(0);
    }
    assert!(!files.is_empty(), "usage: key FILE[@START-END]...");
    for arg in files {
        let (path, span) = match arg.rsplit_once('@').and_then(|(p, s)| Some((p, s.split_once('-')?))) {
            Some((p, (from, to))) => (p.to_owned(), from.parse().ok().zip(to.parse().ok())),
            None => (arg.clone(), None),
        };
        let start = Instant::now();
        let result = if qm { nullamp_audio::compute_qm_key(path.as_ref(), span, &|| false) } else { nullamp_audio::compute_key(path.as_ref(), span, &|| false) };
        let took = start.elapsed();
        match result {
            Ok(Some(k)) => println!("{}{}  confidence {:.2}  {took:.2?}  {path}", NAMES[usize::from(k.tonic)], if k.minor { "m" } else { "" }, k.confidence),
            Ok(None) => println!("no key  {took:.2?}  {path}"),
            Err(e) => println!("{e}  {path}"),
        }
    }
}
