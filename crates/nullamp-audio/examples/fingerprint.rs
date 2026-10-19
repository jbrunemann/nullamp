//! Prints the AcoustID duration and Chromaprint fingerprint of a file, like `fpcalc`.
//!
//! `cargo run --release -p nullamp-audio --example fingerprint FILE`

fn main() {
    let path = std::env::args().nth(1).expect("usage: fingerprint FILE");
    let started = std::time::Instant::now();
    match nullamp_audio::compute_fingerprint(path.as_ref()) {
        Ok(fp) => {
            println!("DURATION={}", fp.duration);
            println!("FINGERPRINT={}", fp.text);
            eprintln!("{:.1}s of audio in {:.0} ms", fp.covered, started.elapsed().as_secs_f64() * 1000.0);
        }
        Err(e) => eprintln!("error: {e}"),
    }
}
