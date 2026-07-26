//! Proves the live-reported freeze actually recovers.
//!
//! Reproduces the exact failure the user hit at the login/lock screen --
//! `SCStream error (System stopped the stream)`, where ScreenCaptureKit tears the
//! stream down but the display remains fully enumerable -- and asserts that
//! frames resume on their own with no restart.
//!
//! This is the case an enumeration-based check is structurally blind to, which is
//! why detection is frame-heartbeat based.
//!
//! cargo run --release -p rusty-capture --features screen-apple --example recovery_after_stream_death

fn main() {
    #[cfg(all(target_os = "macos", feature = "screen-apple"))]
    {
        use rusty_capture::{MacScreenCapturer, ScreenCapturer, types::ScreenConfig};
        use rusty_codecs::traits::VideoSource;
        use std::time::{Duration, Instant};

        let monitors = ScreenCapturer::list_all().expect("list monitors");
        let monitor = monitors
            .iter()
            .find(|m| m.is_primary)
            .or_else(|| monitors.first())
            .expect("a monitor")
            .clone();

        // Concrete type on purpose: the boxed `ScreenCapturer` facade cannot expose
        // the failure-injection seam.
        let mut cap =
            MacScreenCapturer::new(&monitor, &ScreenConfig::default()).expect("open");
        cap.start().expect("start");

        // Phase 1: confirm healthy.
        let mut pre = 0u64;
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(3) {
            if cap.pop_frame().expect("pop").is_some() {
                pre += 1;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        println!("phase 1 (healthy):        {pre} frames in 3s");
        assert!(pre > 30, "capture was not healthy before the test");

        // Phase 2: kill the stream the way the OS does at the lock screen.
        println!("\n--- killing SCStream (simulating 'System stopped the stream') ---");
        cap.__test_kill_stream();

        let mut during = 0u64;
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(2) {
            if cap.pop_frame().expect("pop").is_some() {
                during += 1;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        println!("phase 2 (just after kill): {during} frames in 2s  (expected ~0)");

        // Phase 3: the watchdog needs FRAME_GAP_DEATH (3s) to declare death, then
        // pop_frame rebuilds. Allow generous headroom for detect + rebuild.
        println!("\n--- waiting for automatic recovery ---");
        let mut recovered_at: Option<f64> = None;
        let mut post = 0u64;
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(20) {
            if cap.pop_frame().expect("pop_frame must never error").is_some() {
                if recovered_at.is_none() {
                    recovered_at = Some(t.elapsed().as_secs_f64());
                }
                post += 1;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let _ = cap.stop();

        println!("phase 3 (recovery):       {post} frames");
        match recovered_at {
            Some(s) => println!("  first frame back after {s:.1}s"),
            None => println!("  NEVER recovered"),
        }

        println!("\nVERDICT:");
        if post > 100 {
            println!("  PASS: capture died and recovered ON ITS OWN with no daemon restart.");
            println!("  This is the user-reported lock-screen freeze, fixed and proven.");
        } else {
            println!("  FAIL: capture did not recover ({post} frames after the kill).");
            std::process::exit(1);
        }
    }
    #[cfg(not(all(target_os = "macos", feature = "screen-apple")))]
    println!("requires macOS + screen-apple");
}
