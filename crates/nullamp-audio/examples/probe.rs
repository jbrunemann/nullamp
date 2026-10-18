//! Manual engine check against real hardware: measures load, seek and gapless
//! transition latency and reports underruns. Plays at zero gain unless `--audible`.
//!
//! cargo run --release -p nullamp-audio --example probe -- FILE [NEXT_FILE] [--audible]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nullamp_audio::{AudioEngine, EngineEvent};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let audible = args.iter().any(|a| a == "--audible");
    let files: Vec<PathBuf> = args.iter().filter(|a| !a.starts_with("--")).map(PathBuf::from).collect();
    let file = files.first().expect("usage: probe FILE [NEXT_FILE] [--audible]").clone();

    let (engine, events) = AudioEngine::start(Arc::new(|| {}));
    engine.set_gain(if audible { 0.3 } else { 0.0 });
    let wait = |pred: &dyn Fn(&EngineEvent) -> bool, label: &str| {
        let t = Instant::now();
        loop {
            let ev = events.recv_timeout(Duration::from_secs(10)).unwrap_or_else(|_| panic!("timeout waiting for {label}"));
            if pred(&ev) {
                println!("{label:<28} {:>8.2} ms   {ev:?}", t.elapsed().as_secs_f64() * 1000.0);
                return ev;
            }
            println!("   (other event: {ev:?})");
        }
    };
    // Time until the playhead actually advances past `from`.
    let audible_after = |label: &str, token: u64, from: f64| {
        let t = Instant::now();
        loop {
            let (tok, pos) = engine.position();
            if tok == token && pos > from + 0.001 {
                println!("{label:<28} {:>8.2} ms   position {pos:.3}", t.elapsed().as_secs_f64() * 1000.0);
                return;
            }
            assert!(t.elapsed() < Duration::from_secs(5), "{label}: playhead not moving");
            std::thread::sleep(Duration::from_micros(200));
        }
    };

    std::thread::sleep(Duration::from_millis(300)); // device open
    if args.iter().any(|a| a == "--warm") {
        engine.load(9, file.clone(), 0.0, false);
        wait(&|e| matches!(e, EngineEvent::Loaded { .. } | EngineEvent::LoadFailed { .. }), "warm-up load (paused)");
    }
    let t0 = Instant::now();
    engine.load(1, file.clone(), 0.0, true);
    wait(&|e| matches!(e, EngineEvent::Loaded { .. }), "load -> Loaded");
    audible_after("load -> playhead moving", 1, 0.0);
    println!("total load-to-sound {:.2} ms", t0.elapsed().as_secs_f64() * 1000.0);
    std::thread::sleep(Duration::from_secs(1));

    for target in [600.0, 1500.0, 30.0, 1800.0, 5.0] {
        let t = Instant::now();
        engine.seek(target);
        wait(&|e| matches!(e, EngineEvent::Seeked { .. }), &format!("seek {target} -> Seeked"));
        audible_after(&format!("seek {target} -> moving"), 1, target);
        let _ = t;
        std::thread::sleep(Duration::from_millis(300));
        let (_, pos) = engine.position();
        println!("   position after 300 ms: {pos:.3} (expected ~{:.3})", target + 0.3);
    }

    let t = Instant::now();
    engine.set_paused(true);
    std::thread::sleep(Duration::from_millis(50));
    let (_, p1) = engine.position();
    std::thread::sleep(Duration::from_millis(200));
    let (_, p2) = engine.position();
    println!("pause: playhead frozen = {} ({p1:.3} -> {p2:.3}), {:.1} ms", (p2 - p1).abs() < 1e-6, t.elapsed().as_secs_f64() * 1000.0);
    engine.set_paused(false);
    audible_after("resume -> moving", 1, p2);

    if let Some(next) = files.get(1) {
        // Gapless: seek near the end, preload the next file, measure the transition.
        engine.preload(Some((2, next.clone())));
        let (_, _) = engine.position();
        engine.seek(1e9); // clamps to just before the end
        wait(&|e| matches!(e, EngineEvent::Seeked { .. }), "seek to end");
        wait(&|e| matches!(e, EngineEvent::TrackStarted { .. }), "gapless -> TrackStarted");
        let (tok, pos) = engine.position();
        println!("   now token {tok} at {pos:.3}");
    } else {
        engine.seek(1e9);
        wait(&|e| matches!(e, EngineEvent::Finished { .. }), "end -> Finished");
    }
    println!("underruns: {}", engine.underruns());
}
