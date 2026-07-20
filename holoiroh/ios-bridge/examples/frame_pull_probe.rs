//! Live witness for the FULL video-frame path the iOS app uses:
//! `ticket_connect` -> `subscribe` (blocks on `video_ready`) ->
//! `poll_next_frame` loop, against a REAL running daemon.
//!
//! This is the harness that was missing while "black screen, no error, ever"
//! went undiagnosed: `control_ffi_probe` only exercises ticket parse + the
//! control channel (its "media subscribe OK" is the ticket store, not a
//! video subscription), and `ffi_probe` only exercises error paths -- so the
//! subscribe -> decode -> RGBA8 pull path had never once been witnessed by
//! real execution on any platform.
//!
//! Usage: cargo run --release --example frame_pull_probe -p holoiroh-ios-bridge -- <ticket>
//!
//! Expected on success: "subscribe OK" within a few seconds, then a steady
//! stream of "frame N: WxH (bytes)" lines. Every other outcome is a real
//! finding: a hang before "subscribe OK" means `video_ready()` never
//! resolves; ENDED means the decode pipeline died; endless "no frame yet"
//! means the pipeline runs but never delivers.

use std::ffi::CString;
use std::time::{Duration, Instant};

use holoiroh_ios_bridge::{
    HOLOIROH_ERR_BUFFER_TOO_SMALL, HOLOIROH_ERR_ENDED, HOLOIROH_OK, HoloirohFrame,
    holoiroh_ios_bridge_free, holoiroh_ios_bridge_free_error_string, holoiroh_ios_bridge_new,
    holoiroh_ios_bridge_poll_next_frame, holoiroh_ios_bridge_subscribe,
    holoiroh_ios_bridge_subscription_free, holoiroh_ios_bridge_ticket_connect,
};

fn take_err(err: *mut std::os::raw::c_char) -> String {
    if err.is_null() {
        return "(no error detail)".to_string();
    }
    let s = unsafe { std::ffi::CStr::from_ptr(err) }
        .to_string_lossy()
        .into_owned();
    unsafe { holoiroh_ios_bridge_free_error_string(err) };
    s
}

fn main() {
    // Surface moq-media's decode-pipeline tracing (decoder selection,
    // per-packet decode errors) -- the whole reason this probe exists is
    // that the pipeline fails silently without it. RUST_LOG=info default.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let ticket = std::env::args()
        .nth(1)
        .expect("usage: frame_pull_probe <iroh-live-ticket>");

    let bridge = unsafe { holoiroh_ios_bridge_new() };
    assert!(!bridge.is_null(), "bridge_new returned null");

    let ticket_c = CString::new(ticket).unwrap();
    let mut err = std::ptr::null_mut();
    let status = unsafe { holoiroh_ios_bridge_ticket_connect(bridge, ticket_c.as_ptr(), &mut err) };
    if status != HOLOIROH_OK {
        eprintln!("ticket_connect failed ({status}): {}", take_err(err));
        std::process::exit(1);
    }
    println!("ticket_connect OK");

    // The exact call the iOS app's IrohLiveFrameSource makes -- blocks until
    // the broadcast's catalog advertises a video rendition. A hang here IS a
    // finding (video_ready never resolving), so print a marker first.
    println!("subscribing (blocks until a video rendition appears)...");
    let t0 = Instant::now();
    let mut err = std::ptr::null_mut();
    let subscription = unsafe { holoiroh_ios_bridge_subscribe(bridge, &mut err) };
    if subscription.is_null() {
        eprintln!("subscribe failed: {}", take_err(err));
        unsafe { holoiroh_ios_bridge_free(bridge) };
        std::process::exit(1);
    }
    println!("subscribe OK ({:?})", t0.elapsed());

    // Poll loop, same shape as IrohLiveFrameSource.pollLoop: grow-on-demand
    // scratch buffer, ~60Hz cadence, 15s total window.
    let mut buf: Vec<u8> = Vec::new();
    let mut frames = 0u64;
    let mut last_dims = (0u32, 0u32);
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        let mut frame = HoloirohFrame {
            width: 0,
            height: 0,
            timestamp_us: 0,
            pixel_format: 0,
            kind: 0,
        };
        let written = unsafe {
            holoiroh_ios_bridge_poll_next_frame(
                subscription,
                if buf.is_empty() {
                    std::ptr::null_mut()
                } else {
                    buf.as_mut_ptr()
                },
                buf.len(),
                &mut frame,
            )
        };
        if written > 0 {
            frames += 1;
            if frames <= 3 || (frame.width, frame.height) != last_dims {
                println!(
                    "frame {frames}: {}x{} ({} bytes, ts={}us)",
                    frame.width, frame.height, written, frame.timestamp_us
                );
            }
            last_dims = (frame.width, frame.height);
        } else if written == 0 {
            std::thread::sleep(Duration::from_millis(16));
        } else if written == HOLOIROH_ERR_BUFFER_TOO_SMALL {
            let needed = frame.width as usize * frame.height as usize * 4;
            println!(
                "resizing buffer to {needed} bytes for {}x{}",
                frame.width, frame.height
            );
            buf.resize(needed, 0);
        } else if written == HOLOIROH_ERR_ENDED {
            println!("track ENDED after {frames} frames");
            break;
        } else {
            println!("poll error {written} after {frames} frames");
            break;
        }
    }

    println!(
        "TOTAL: {frames} frames in {:?} ({}x{})",
        t0.elapsed(),
        last_dims.0,
        last_dims.1
    );

    unsafe {
        holoiroh_ios_bridge_subscription_free(subscription);
        holoiroh_ios_bridge_free(bridge);
    }
    println!("probe complete");
    if frames == 0 {
        std::process::exit(2);
    }
}
