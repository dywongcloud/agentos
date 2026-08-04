//! Offline witness for clarification's empty-input short circuit.
//!
//!   cargo run --example clarify_degenerate_probe -p holoiroh-daemon

use holoiroh_daemon::clarify::generate_clarifying_questions_if_configured;

#[tokio::main]
async fn main() {
    let empty = generate_clarifying_questions_if_configured("", None).await;
    assert!(empty.is_empty(), "empty prompt must yield no questions");

    let whitespace = generate_clarifying_questions_if_configured("   \n\t  ", None).await;
    assert!(
        whitespace.is_empty(),
        "whitespace-only prompt must yield no questions"
    );

    println!(
        "clarify_degenerate_probe: OK -- empty and whitespace prompts returned zero questions without a client, API key, discovery, attestation, or inference"
    );
}
