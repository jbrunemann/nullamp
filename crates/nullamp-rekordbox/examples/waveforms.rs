//! `cargo run --release -p nullamp-rekordbox --example waveforms -- /Volumes/STICK`
//!
//! Lists the stick's tracks and checks each waveform against our own decode of the audio file:
//! Pearson correlation of the peak curves over 256 buckets (1.0 is identical).

use std::path::Path;

fn correlation(a: &[u8], b: &[u8]) -> f32 {
    let n = a.len().min(b.len()) as f32;
    let mean = |v: &[u8]| v.iter().map(|&x| f32::from(x)).sum::<f32>() / n;
    let (ma, mb) = (mean(a), mean(b));
    let (mut cov, mut va, mut vb) = (0.0, 0.0, 0.0);
    for (&x, &y) in a.iter().zip(b) {
        let (dx, dy) = (f32::from(x) - ma, f32::from(y) - mb);
        cov += dx * dy;
        va += dx * dx;
        vb += dy * dy;
    }
    cov / (va * vb).sqrt()
}

fn main() {
    let Some(root) = std::env::args().nth(1) else {
        eprintln!("usage: waveforms STICK_ROOT");
        return;
    };
    let stick = match nullamp_rekordbox::Stick::open(Path::new(&root)) {
        Ok(stick) => stick,
        Err(e) => return eprintln!("{root}: {e}"),
    };
    for t in &stick.tracks {
        println!("{} - {} | {} | {:?} | {:?} BPM | {} s", t.artist, t.title, t.album, t.key, t.bpm, t.duration_secs);
        let theirs = match t.waveform() {
            Ok(Some(w)) => w,
            Ok(None) => {
                println!("  no waveform");
                continue;
            }
            Err(e) => {
                println!("  waveform: {e}");
                continue;
            }
        };
        let started = std::time::Instant::now();
        let ours = match nullamp_audio::compute_waveform(&t.file, &|| false) {
            Ok(w) => w,
            Err(e) => {
                println!("  {} columns, audio not decoded: {e}", theirs.heights.len());
                continue;
            }
        };
        let (peak, _) = theirs.buckets(256);
        let n = nullamp_audio::WAVEFORM_BUCKETS;
        let ours256: Vec<u8> = (0..256).map(|i| ours.peak[i * n / 256..(i + 1) * n / 256].iter().copied().max().unwrap_or(0)).collect();
        println!("  {} columns, correlation with our decode {:.3} (decode took {:?})", theirs.heights.len(), correlation(&peak, &ours256), started.elapsed());
    }
}
