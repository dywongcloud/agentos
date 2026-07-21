//! Pure-logic CI witness for issue-1: the task-completion framing guidance the
//! daemon injects into EVERY prompt (see `crate::agent_guidance`, wired into
//! `HoloControlBridge::run_prompt`'s `augmented_text`) actually carries the
//! "pre-existing similar content is not completion" rule that fixes the
//! "say hi to the design team on Slack when a hi already exists" failure.
//! Deterministic, no backend/TCC/network needed.
//!
//! Run with `cargo run --example task_framing_probe -p holoiroh-daemon`.

use holoiroh_daemon::agent_guidance::{task_framing_block, TASK_FRAMING_MARKER};

fn main() {
    let block = task_framing_block();
    println!("--- injected task-framing guidance ---\n{block}\n");

    assert!(
        block.contains(TASK_FRAMING_MARKER),
        "guidance is missing its own marker line: {TASK_FRAMING_MARKER:?}"
    );
    assert!(
        block.contains("Slack"),
        "guidance should name the concrete Slack example so the agent generalizes it"
    );
    assert!(
        block.contains("Perform the new action the user requested"),
        "guidance must tell the agent to ACT, not just check whether it's done"
    );
    assert!(
        block.to_lowercase().contains("instruction to act"),
        "guidance must frame a request as an instruction to act"
    );

    println!(
        "task_framing_probe: OK -- every turn is told to complete the requested action even when \
         similar prior content already exists."
    );
}
