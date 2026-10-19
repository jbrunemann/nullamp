//! Times the waveform pass: `cargo run --release -p nullamp-audio --example waveform FILE`

use std::time::Instant;

fn main() {
    let path = std::env::args().nth(1).expect("usage: waveform FILE");
    let start = Instant::now();
    let wave = nullamp_audio::compute_waveform(path.as_ref(), &|| false).expect("waveform");
    let took = start.elapsed();
    let bars: String = wave.rms.chunks(wave.rms.len().div_ceil(64)).map(|c| " .:-=+*#%@".as_bytes()[usize::from(*c.iter().max().unwrap()) * 9 / 255] as char).collect();
    println!("{} buckets in {:.2?}\n{bars}", wave.peak.len(), took);
}
