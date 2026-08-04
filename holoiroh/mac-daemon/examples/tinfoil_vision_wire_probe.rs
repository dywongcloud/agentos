//! Pure-logic CI witness for the image-analysis wire messages (`ClientMessage::AnalyzeImage` +
//! `ServerMessage::ImageAnalyzed`/`ImageAnalysisFailed`) and the non-image-MIME rejection path
//! `control_channel.rs`'s `AnalyzeImage` handler relies on (`image::load_from_memory` on
//! garbage bytes). No network call anywhere in this probe.
//!
//!   cargo run --example tinfoil_vision_wire_probe -p holoiroh-daemon

use holoiroh_daemon::control_channel::{ClientMessage, ServerMessage};

fn main() {
    let request = ClientMessage::AnalyzeImage {
        request_id: "req-2".to_string(),
        image_data_base64: "AAAA".to_string(),
        prompt: "what is in this screenshot?".to_string(),
    };
    let rj = serde_json::to_string(&request).expect("serialize request");
    assert!(
        rj.contains("\"type\":\"analyze_image\""),
        "wrong type tag: {rj}"
    );
    let back: ClientMessage = serde_json::from_str(&rj).expect("deserialize request");
    assert_eq!(back, request);

    let ok = ServerMessage::ImageAnalyzed {
        request_id: "req-2".to_string(),
        text: "A login form.".to_string(),
    };
    let oj = serde_json::to_string(&ok).expect("serialize ok");
    assert!(
        oj.contains("\"type\":\"image_analyzed\""),
        "wrong type tag: {oj}"
    );
    let back_ok: ServerMessage = serde_json::from_str(&oj).expect("deserialize ok");
    assert_eq!(back_ok, ok);

    let failed = ServerMessage::ImageAnalysisFailed {
        request_id: "req-2".to_string(),
        error: "bad image".to_string(),
    };
    let fj = serde_json::to_string(&failed).expect("serialize failed");
    assert!(
        fj.contains("\"type\":\"image_analysis_failed\""),
        "wrong type tag: {fj}"
    );
    println!("wire round-trip: OK");

    // Non-image MIME: control_channel.rs's AnalyzeImage handler decodes with
    // `image::load_from_memory` before ever calling `tinfoil_vision::analyze_image`; this is
    // the exact rejection path it depends on, exercised directly and deterministically.
    let garbage = b"this is definitely not an image file, just plain text bytes";
    let result = image::load_from_memory(garbage);
    assert!(
        result.is_err(),
        "garbage bytes must fail image decoding, not panic"
    );
    println!(
        "non-image bytes -> decode error (as expected): {:?}",
        result.err()
    );

    println!(
        "tinfoil_vision_wire_probe: OK -- wire shapes round-trip and non-image input is rejected before any network call."
    );
}
