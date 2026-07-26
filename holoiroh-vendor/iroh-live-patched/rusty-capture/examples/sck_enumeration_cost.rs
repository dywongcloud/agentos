//! Measures how expensive `SCShareableContent::get()` actually is.
//!
//! `MacScreenCapturer::try_recover_if_target_gone` calls it from `pop_frame()`,
//! i.e. on the SAME thread that delivers every video frame. If the call blocks
//! for a meaningful fraction of a frame interval (33ms at 30fps / 16.7ms at
//! 60fps), the recovery check is itself a periodic frame-delivery stall -- a
//! visible hitch every 10s, a self-inflicted regression on the exact smoothness
//! the recovery was added to protect.
//!
//! Run: cargo run --release -p rusty-capture --features screen-apple --example sck_enumeration_cost

fn main() {
    #[cfg(all(target_os = "macos", feature = "screen-apple"))]
    {
        use screencapturekit::shareable_content::SCShareableContent;
        use std::time::Instant;

        println!("measuring SCShareableContent::get() -- the call try_recover_if_target_gone makes");
        println!("frame budget: 33.3ms @30fps, 16.7ms @60fps\n");

        let mut samples = Vec::new();
        for i in 1..=10 {
            let t = Instant::now();
            let content = SCShareableContent::get();
            let elapsed = t.elapsed();
            let (ok, displays, windows) = match &content {
                Ok(c) => (true, c.displays().len(), c.windows().len()),
                Err(_) => (false, 0, 0),
            };
            let ms = elapsed.as_secs_f64() * 1000.0;
            println!("  sample {i:2}: {ms:>8.2}ms  ok={ok} displays={displays} windows={windows}");
            samples.push(ms);
        }

        samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let min = samples[0];
        let max = samples[samples.len() - 1];
        let median = samples[samples.len() / 2];
        let mean = samples.iter().sum::<f64>() / samples.len() as f64;
        println!("\n  min={min:.2}ms median={median:.2}ms mean={mean:.2}ms max={max:.2}ms");

        println!("\nVERDICT:");
        if max > 16.7 {
            println!("  PROBLEM: worst case {max:.2}ms exceeds a 60fps frame budget. Calling this");
            println!("  synchronously on the frame-delivery thread stalls video every recovery tick.");
            println!("  Move the check off the poll thread (dedicated thread + atomic flag).");
        } else if max > 8.0 {
            println!("  MARGINAL: worst case {max:.2}ms is a large fraction of a frame budget;");
            println!("  on a loaded system this could visibly hitch. Off-thread is safer.");
        } else {
            println!("  ACCEPTABLE: worst case {max:.2}ms is well under a frame budget.");
        }
    }
    #[cfg(not(all(target_os = "macos", feature = "screen-apple")))]
    println!("requires macOS + screen-apple feature");
}
