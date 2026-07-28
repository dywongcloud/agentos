//! Witness for graceful error handling across the four new Tinfoil modules: a bogus API key
//! against the REAL endpoint (a genuine network round trip that fails, same class of test as
//! `clarify_degenerate_probe.rs`'s bogus-key case) must yield `Err`, never a panic or hang; and
//! `tinfoil_documents::parse_convert_response` must reject malformed JSON as `Err`, never panic,
//! purely locally (no network). Local live probe (network + a deliberately-wrong key), not CI.
//!
//!   cargo run --example tinfoil_error_handling_probe -p holoiroh-daemon

use holoiroh_daemon::tinfoil_audio::{speech, transcribe};
use holoiroh_daemon::tinfoil_documents::{convert_documents, parse_convert_response, ConvertMode, DocumentInput};
use holoiroh_daemon::tinfoil_planner::plan_task;
use holoiroh_daemon::tinfoil_vision::{analyze_image, VisionModel};

const BOGUS_KEY: &str = "tk_not-a-real-key-00000000000000000000000000";

#[tokio::main]
async fn main() {
    // --- network failure (real round trip, bogus key): every module must fail gracefully ---
    let start = std::time::Instant::now();

    let files = vec![DocumentInput { filename: "x.txt".to_string(), bytes: vec![1, 2, 3] }];
    let err = convert_documents(BOGUS_KEY, &files, ConvertMode::Text)
        .await
        .expect_err("bogus key must fail, not succeed");
    println!("documents + bogus key -> Err (as expected): {err}");

    let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(8, 8, image::Rgba([0, 0, 0, 255])));
    let err = analyze_image(BOGUS_KEY, &img, "describe this", VisionModel::Gemma431b)
        .await
        .expect_err("bogus key must fail, not succeed");
    println!("vision + bogus key -> Err (as expected): {err}");

    let err = transcribe(BOGUS_KEY, vec![0u8; 100], "x.wav")
        .await
        .expect_err("bogus key must fail, not succeed");
    println!("audio transcribe + bogus key -> Err (as expected): {err}");

    let err = speech(BOGUS_KEY, "hello", "serena")
        .await
        .expect_err("bogus key must fail, not succeed");
    println!("audio speech + bogus key -> Err (as expected): {err}");

    let err = plan_task(BOGUS_KEY, "do something")
        .await
        .expect_err("bogus key must fail, not succeed");
    println!("planner + bogus key -> Err (as expected): {err}");

    let elapsed = start.elapsed();
    assert!(elapsed.as_secs() < 60, "every bogus-key call must fail fast, not hang: {elapsed:?}");
    println!("all 5 bogus-key calls failed gracefully in {elapsed:?} total");

    // --- malformed response handling (pure, local, no network) ---
    let err = parse_convert_response("not json at all").expect_err("garbage must not parse");
    println!("garbage body -> Err (as expected): {err}");

    let err = parse_convert_response(r#"{"totally": "unexpected shape"}"#)
        .expect_err("a valid-JSON-but-wrong-shape body must not silently succeed");
    println!("wrong-shape JSON -> Err (as expected): {err}");

    let err = parse_convert_response("").expect_err("empty body must not parse");
    println!("empty body -> Err (as expected): {err}");

    let ok = parse_convert_response(r#"{"document":{"md_content":"hello world"},"status":"success"}"#)
        .expect("the real confirmed single-document shape must still parse");
    assert_eq!(ok.len(), 1);
    assert_eq!(ok[0].markdown, "hello world");
    println!("real confirmed shape -> Ok (as expected): {:?}", ok[0].markdown);

    println!(
        "tinfoil_error_handling_probe: OK -- every module fails gracefully on a bad key, and malformed/wrong-shape JSON is rejected without panicking."
    );
}
