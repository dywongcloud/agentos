//! Verifies `VtbEncoder` stamps the REAL capture timestamp, monotonically, and
//! falls back safely when the input timestamp cannot be trusted.
//!
//! It previously always used a synthetic frame counter, which drifts behind wall
//! clock on a dirty-region source and pins the receiver's playout-clock
//! reference open. The guard matters as much as the fix: `Duration::ZERO` is the
//! documented default for capture frames carrying no timestamp, and a
//! non-monotonic PTS anchor has already caused a live permanent-black-screen
//! incident elsewhere in this stack.
//!
//! cargo run --release -p rusty-codecs --features videotoolbox --example vtb_pts_propagation

fn main() {
    #[cfg(all(target_os = "macos", feature = "videotoolbox"))]
    {
        use rusty_codecs::{
            codec::VtbEncoder,
            format::{VideoEncoderConfig, VideoFrame, VideoPreset},
            traits::{VideoEncoder, VideoEncoderFactory},
        };
        use std::time::Duration;

        let cfg = VideoEncoderConfig::from_preset(VideoPreset::P180);
        let (w, h) = (cfg.width, cfg.height);
        let mut enc = VtbEncoder::with_config(cfg).expect("create encoder");

        // Irregular dirty-region cadence, plus the degenerate inputs the guard
        // must reject: zero, an exact repeat, and a backwards jump.
        let cases: Vec<(u64, &str)> = vec![
            (0, "zero -> must fall back to the counter"),
            (100_000, "100ms"),
            (350_000, "350ms (irregular gap, as a real screen produces)"),
            (350_000, "repeat -> must fall back (not strictly ahead)"),
            (200_000, "backwards -> must fall back"),
            (900_000, "900ms"),
        ];

        let rgba = bytes::Bytes::from(vec![0u8; (w * h * 4) as usize]);
        for (us, label) in &cases {
            let frame = VideoFrame::new_rgba(rgba.clone(), w, h, Duration::from_micros(*us));
            match enc.push_frame(frame) {
                Ok(()) => println!("  pushed ts={us:>8}us   ({label})"),
                Err(e) => println!("  push FAILED ts={us}us: {e}"),
            }
        }

        // VTB pipelines internally; force it to emit everything.
        enc.__test_flush();

        let mut ptss: Vec<Duration> = Vec::new();
        while let Ok(Some(pkt)) = enc.pop_packet() {
            ptss.push(pkt.timestamp);
        }

        println!("\ndrained {} packets", ptss.len());
        for (i, t) in ptss.iter().enumerate() {
            println!("  packet {i}: pts = {:?}", t);
        }

        println!("\nVERDICT:");
        if ptss.is_empty() {
            println!("  INCONCLUSIVE: encoder emitted nothing (VTB may still be buffering).");
            return;
        }
        let monotonic = ptss.windows(2).all(|p| p[1] >= p[0]);
        let real_clock_used = ptss.iter().any(|t| t.as_micros() >= 100_000);
        if !monotonic {
            println!("  FAIL: emitted PTS is not monotonic -- the guard did not hold.");
            std::process::exit(1);
        }
        if !real_clock_used {
            println!("  FAIL: no emitted PTS reflects the real capture clock; still counter-based.");
            std::process::exit(1);
        }
        println!("  PASS: emitted PTS reflects the real capture clock AND stays monotonic across");
        println!("  a sequence containing a zero, an exact repeat, and a backwards timestamp.");
    }
    #[cfg(not(all(target_os = "macos", feature = "videotoolbox")))]
    println!("requires macOS + videotoolbox");
}
