//! Does ScreenCaptureKit deliver frames on a STATIC screen?
//!
//! This decides whether "no frame for N seconds" is a valid liveness signal.
//! If SCK is purely dirty-region-driven, a static desktop legitimately produces
//! no frames and a frame-gap watchdog would rebuild healthy streams. If SCK
//! instead delivers at ~the configured rate regardless, frame arrival is a free
//! and far better death signal than target enumeration -- and critically it
//! catches the "stream stopped but display still enumerable" case, which is
//! exactly the `SCStream error (System stopped the stream)` failure observed at
//! the login/lock screen.
//!
//! Run and DO NOT TOUCH the machine while it runs.
//!
//! cargo run --release -p rusty-capture --features screen-apple --example frame_arrival_pattern

fn main() {
    #[cfg(all(target_os = "macos", feature = "screen-apple"))]
    {
        use rusty_capture::ScreenCapturer;
        use rusty_codecs::traits::VideoSource;
        use std::time::{Duration, Instant};

        let monitors = ScreenCapturer::list_all().expect("list monitors");
        let monitor = monitors
            .iter()
            .find(|m| m.is_primary)
            .or_else(|| monitors.first())
            .expect("a monitor")
            .clone();

        let mut cap = ScreenCapturer::with_monitor(&monitor).expect("open");
        cap.start().expect("start");

        let run_for = Duration::from_secs(20);
        let started = Instant::now();
        let mut last_frame = Instant::now();
        let mut gaps_ms: Vec<f64> = Vec::new();
        let mut frames = 0u64;

        while started.elapsed() < run_for {
            if cap.pop_frame().expect("pop").is_some() {
                let now = Instant::now();
                gaps_ms.push(now.duration_since(last_frame).as_secs_f64() * 1000.0);
                last_frame = now;
                frames += 1;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let _ = cap.stop();

        let secs = started.elapsed().as_secs_f64();
        gaps_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let max_gap = gaps_ms.last().copied().unwrap_or(f64::INFINITY);
        let median_gap = if gaps_ms.is_empty() { f64::INFINITY } else { gaps_ms[gaps_ms.len() / 2] };
        let p99 = if gaps_ms.is_empty() { f64::INFINITY } else { gaps_ms[(gaps_ms.len() * 99 / 100).min(gaps_ms.len() - 1)] };

        println!("ran {secs:.1}s  frames={frames} ({:.1} fps)", frames as f64 / secs);
        println!("  inter-frame gap: median={median_gap:.1}ms  p99={p99:.1}ms  MAX={max_gap:.1}ms");

        println!("\nVERDICT:");
        if frames == 0 {
            println!("  SCK delivered NOTHING on a static screen -> frame-gap is NOT a usable");
            println!("  liveness signal on its own.");
        } else if max_gap < 1000.0 {
            println!("  SCK delivers continuously even on a static screen (max gap {max_gap:.0}ms).");
            println!("  => A frame-gap watchdog IS reliable, is free, and catches 'stream stopped");
            println!("     but display still enumerable' -- which target-enumeration CANNOT see.");
            println!("     Recommended threshold: ~{:.0}ms (>10x observed max gap).", (max_gap * 10.0).max(3000.0));
        } else {
            println!("  Max gap {max_gap:.0}ms on a static screen -> a frame-gap watchdog needs a");
            println!("  threshold well above that to avoid false rebuilds.");
        }
    }
    #[cfg(not(all(target_os = "macos", feature = "screen-apple")))]
    println!("requires macOS + screen-apple");
}
