//! Pure-logic CI witness for the self-correction guidance the daemon injects
//! into EVERY prompt (see `crate::agent_guidance`, wired into
//! `HoloControlBridge::run_prompt`'s `augmented_text`): the agent must detect
//! its own mistakes (text in the wrong field, wrong click target, unexpected
//! state) and fix just that step instead of freezing or restarting the whole
//! task -- fixes the reported "email subject landed in the recipients field,
//! agent stalled" failure. Deterministic, no backend/TCC/network needed.
//!
//! Run with `cargo run --example self_correction_probe -p holoiroh-daemon`.

use holoiroh_daemon::agent_guidance::{SELF_CORRECTION_MARKER, task_framing_block};

fn main() {
    let block = task_framing_block();
    println!("--- injected task-framing guidance ---\n{block}\n");

    assert!(
        block.contains(SELF_CORRECTION_MARKER),
        "guidance is missing its own self-correction marker line: {SELF_CORRECTION_MARKER:?}"
    );
    assert!(
        block.to_lowercase().contains("do not freeze"),
        "guidance must explicitly forbid freezing/stalling on a detected mistake"
    );
    assert!(
        block.to_lowercase().contains("do not restart the whole task"),
        "guidance must forbid restarting the entire task over a one-step mistake"
    );
    assert!(
        block.to_lowercase().contains("wrong field"),
        "guidance should name the concrete wrong-field example so the agent generalizes it"
    );
    assert!(
        block.to_lowercase().contains("ask the user"),
        "guidance must still allow asking the user as a genuine last resort"
    );

    println!(
        "self_correction_probe: OK -- every turn is told to detect and fix its own mistakes \
         instead of stalling or restarting."
    );
}
