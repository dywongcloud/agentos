//! Adversarial witness for the clarify module's degenerate-input handling:
//! empty/whitespace prompt, and a bogus API key (simulating network/auth
//! failure) both must yield an EMPTY question list, never a panic or hang.
//! Pure-logic-adjacent (one real network call for the bogus-key case, against
//! the real Tinfoil endpoint, expected to 401/fail fast) -- local only, not CI
//! (network-dependent), same status as the other clarify live probes.
//!
//!   cargo run --example clarify_degenerate_probe -p holoiroh-daemon

use holoiroh_daemon::clarify::{ClarifyConfig, generate_clarifying_questions};

#[tokio::main]
async fn main() {
    // Empty/whitespace prompt: must short-circuit to empty without any network call.
    let bogus_config = ClarifyConfig::new("not-a-real-key-xxxxxxxxxxxxxxxxxxxx".to_string());
    let empty = generate_clarifying_questions("", &bogus_config).await;
    assert!(empty.is_empty(), "empty prompt must yield no questions");
    println!("empty prompt -> {} question(s) (expected 0)", empty.len());

    let whitespace = generate_clarifying_questions("   \n\t  ", &bogus_config).await;
    assert!(whitespace.is_empty(), "whitespace-only prompt must yield no questions");
    println!("whitespace prompt -> {} question(s) (expected 0)", whitespace.len());

    // Bogus API key against the REAL endpoint: must fail gracefully (401/network),
    // never panic, never hang past the internal 20s timeout.
    let start = std::time::Instant::now();
    let bad_key = generate_clarifying_questions("send a message to the team", &bogus_config).await;
    let elapsed = start.elapsed();
    assert!(bad_key.is_empty(), "bogus key must yield no questions, not an error/panic");
    assert!(elapsed.as_secs() < 25, "must fail fast, not hang past the internal timeout: {elapsed:?}");
    println!("bogus key -> {} question(s) in {:?} (expected 0, fast)", bad_key.len(), elapsed);

    // Very long prompt: capped internally (4000 chars) before the request is built --
    // exercise it against the bogus key too (still must not panic on the cap logic).
    let long_prompt = "do something ".repeat(2000);
    println!("long prompt length: {} chars", long_prompt.len());
    let long_result = generate_clarifying_questions(&long_prompt, &bogus_config).await;
    assert!(long_result.is_empty(), "long prompt with bogus key -> no questions, no panic");
    println!("long prompt -> {} question(s) (expected 0, no panic on the 4000-char cap)", long_result.len());

    println!(
        "clarify_degenerate_probe: OK -- empty/whitespace short-circuit, bogus key fails fast+empty, long prompt capped without panic"
    );
}
