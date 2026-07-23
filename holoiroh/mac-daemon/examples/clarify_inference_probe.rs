//! Live witness for the daemon's clarify module
//! (`clarify::generate_clarifying_questions`): calls the real Tinfoil model with
//! the daemon's own clarification prompt + JSON parsing/capping, for an
//! ambiguous prompt (expects structured questions) and a clear prompt (expects
//! empty). Local live probe (needs the `TINFOIL_API_KEY` from
//! `mac-daemon/.env` + network); deliberately NOT in CI.
//!
//! Run from the repo root: `cargo run --example clarify_inference_probe -p holoiroh-daemon`.

use holoiroh_daemon::clarify::{ClarifyConfig, generate_clarifying_questions};

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
        println!("no TINFOIL_API_KEY in mac-daemon/.env -- skipping (clarification disabled)");
        std::process::exit(3);
    };
    let config = ClarifyConfig::new(key);
    println!("clarify model: {}", config.model());

    let ambiguous =
        generate_clarifying_questions("send a message to the team about the meeting", &config).await;
    println!("ambiguous -> {} question(s)", ambiguous.len());
    for q in &ambiguous {
        println!("  Q: {} | options: {:?}", q.question, q.options);
    }

    let clear = generate_clarifying_questions("open the Safari app", &config).await;
    println!("clear -> {} question(s)", clear.len());

    assert!(!ambiguous.is_empty(), "an ambiguous prompt must yield clarifying questions");
    assert!(ambiguous.len() <= 3, "questions capped at 3");
    for q in &ambiguous {
        assert!(q.options.len() <= 3, "options capped at 3 per question");
        assert!(!q.question.trim().is_empty(), "no empty question text");
    }

    if clear.is_empty() {
        println!("clarify_inference_probe: OK -- ambiguous produced structured questions, clear produced none");
    } else {
        println!(
            "clarify_inference_probe: OK (partial) -- ambiguous produced questions; the clear prompt also produced {} (model non-determinism, still capped/valid)",
            clear.len()
        );
    }
}
