//! Regression probe for the frame-delivery stall the target-watchdog fix exists to prevent.
//!
//! `MacScreenCapturer::pop_frame` runs on `SharedVideoSource`'s poll thread --
//! the thread that delivers every video frame. An earlier version of the
//! lock-screen recovery logic called `SCShareableContent::get()` (measured
//! 37-54ms, see `sck_enumeration_cost.rs`) inline from that method every 10s,
//! which is longer than a 30fps frame: a visible hitch every 10 seconds.
//!
//! This runs a real capturer for longer than one full watchdog interval and
//! asserts no single `pop_frame` call ever approaches that cost.
//!
//! Run: cargo run --release -p rusty-capture --features screen-apple --example pop_frame_latency

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
            .expect("at least one monitor")
            .clone();
        println!("capturing: {}", monitor.summary());

        let mut cap =
            ScreenCapturer::with_monitor(&monitor).expect("open capturer");
        cap.start().expect("start");

        // Longer than TARGET_WATCHDOG_INTERVAL (10s) so the watchdog definitely
        // fires at least once during the measurement window.
        let run_for = Duration::from_secs(14);
        let started = Instant::now();
        let mut calls = 0u64;
        let mut frames = 0u64;
        let mut max_ms = 0.0f64;
        let mut over_16ms = 0u64;
        let mut over_33ms = 0u64;

        while started.elapsed() < run_for {
            let t = Instant::now();
            let got = cap.pop_frame().expect("pop_frame must not error");
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            calls += 1;
            if got.is_some() {
                frames += 1;
            }
            if ms > max_ms {
                max_ms = ms;
            }
            if ms > 16.7 {
                over_16ms += 1;
            }
            if ms > 33.3 {
                over_33ms += 1;
            }
            std::thread::sleep(Duration::from_millis(2));
        }

        let _ = cap.stop();
        let secs = started.elapsed().as_secs_f64();
        println!(
            "\nran {secs:.1}s  pop_frame calls={calls}  frames={frames} ({:.1} fps)",
            frames as f64 / secs
        );
        println!("  worst pop_frame: {max_ms:.2}ms");
        println!("  calls over 16.7ms (60fps budget): {over_16ms}");
        println!("  calls over 33.3ms (30fps budget): {over_33ms}");

        println!("\nVERDICT:");
        if over_33ms > 0 {
            println!("  FAIL: {over_33ms} call(s) exceeded a 30fps frame budget -- the enumeration");
            println!("  is still stalling the frame-delivery thread.");
            std::process::exit(1);
        } else if max_ms > 16.7 {
            println!("  MARGINAL: worst {max_ms:.2}ms exceeded a 60fps budget (none exceeded 30fps).");
        } else {
            println!("  PASS: worst pop_frame {max_ms:.2}ms, well inside a frame budget, across a");
            println!("  full watchdog interval. The ~40ms enumeration is off the delivery thread.");
        }
    }
    #[cfg(not(all(target_os = "macos", feature = "screen-apple")))]
    println!("requires macOS + screen-apple feature");
}
