//! Live witness for the four new Tinfoil modules
//! (`tinfoil_documents`/`tinfoil_vision`/`tinfoil_audio`/`tinfoil_planner`): calls each real
//! endpoint once with the daemon's own configured key, proving the hand-written request/response
//! shapes actually match Tinfoil's live API (the document-conversion response envelope in
//! particular was inferred from docs, not previously confirmed against a real response). Local
//! live probe (needs `TINFOIL_API_KEY` from `mac-daemon/.env` + network); deliberately NOT in CI,
//! same posture as `clarify_inference_probe.rs`.
//!
//! Run from the repo root: `cargo run --example tinfoil_live_probe -p holoiroh-daemon`.

use holoiroh_daemon::tinfoil_audio::{speech, transcribe};
use holoiroh_daemon::tinfoil_documents::{convert_documents, ConvertMode, DocumentInput};
use holoiroh_daemon::tinfoil_planner::plan_task;
use holoiroh_daemon::tinfoil_vision::{analyze_image, VisionModel};

fn load_key() -> Option<String> {
    let env = std::fs::read_to_string("mac-daemon/.env").ok()?;
    for line in env.lines() {
        if let Some(rest) = line.trim().strip_prefix("TINFOIL_API_KEY=") {
            let key = rest.trim().trim_matches('"').trim_matches('\'').to_string();
            if !key.is_empty() {
                return Some(key);
            }
        }
    }
    None
}

#[tokio::main]
async fn main() {
    let Some(key) = load_key() else {
        println!("no TINFOIL_API_KEY in mac-daemon/.env -- skipping live probe");
        return;
    };

    // --- documents: a tiny plain-text file, forced through the CSV/text path ---
    let files = vec![DocumentInput {
        filename: "probe.csv".to_string(),
        bytes: b"name,role\nAda,Engineer\n".to_vec(),
    }];
    match convert_documents(&key, &files, ConvertMode::Text).await {
        Ok(docs) => {
            assert!(!docs.is_empty(), "expected at least one converted document");
            println!("documents: OK -- {} doc(s), first markdown: {:.200}", docs.len(), docs[0].markdown);
        }
        Err(err) => println!("documents: FAILED -- {err:#}"),
    }

    // --- vision: a tiny synthetic red square, real gemma4-31b call ---
    let mut canvas = image::RgbaImage::from_pixel(64, 64, image::Rgba([220, 20, 20, 255]));
    for pixel in canvas.pixels_mut() {
        *pixel = image::Rgba([220, 20, 20, 255]);
    }
    let img = image::DynamicImage::ImageRgba8(canvas);
    match analyze_image(&key, &img, "What color is this image? Answer in one word.", VisionModel::Gemma431b).await {
        Ok(text) => println!("vision: OK -- {text:.200}"),
        Err(err) => println!("vision: FAILED -- {err:#}"),
    }

    // --- audio speech: real qwen3-tts call, then feed the WAV straight back through transcribe ---
    match speech(&key, "Testing one two three.", "serena").await {
        Ok(wav) => {
            println!("speech: OK -- {} WAV bytes", wav.len());
            match transcribe(&key, wav, "probe.wav").await {
                Ok(text) => println!("transcribe (round-trip of our own TTS output): OK -- {text:.200}"),
                Err(err) => println!("transcribe: FAILED -- {err:#}"),
            }
        }
        Err(err) => println!("speech: FAILED -- {err:#}"),
    }

    // --- planner: real glm-5.2 tool-calling call ---
    match plan_task(&key, "Open Safari and check the weather").await {
        Ok(steps) => {
            assert!(!steps.is_empty(), "expected at least one plan step");
            println!("planner: OK -- {} step(s): {:?}", steps.len(), steps);
        }
        Err(err) => println!("planner: FAILED -- {err:#}"),
    }

    println!("tinfoil_live_probe: done -- see FAILED lines above for anything that didn't work against the real API.");
}
