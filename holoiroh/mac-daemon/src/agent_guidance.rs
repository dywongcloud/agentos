//! Unconditional, per-turn task-execution guidance injected into every prompt
//! the daemon forwards to `holo serve` (see `crate::holo_bridge::control`'s
//! `run_prompt`).
//!
//! Distinct from the two neighbouring injection surfaces:
//! - `crate::process_awareness`'s guard block is a SAFETY rule about the
//!   *environment* ("never interrupt an existing Claude Code session").
//! - `crate::env_context`'s facts are semantically retrieved (top-k, may not
//!   surface on a given turn).
//!
//! This block is about HOW to carry out the user's request, and -- like the
//! guard block -- is injected verbatim on EVERY turn so the behaviour can never
//! silently drop out.
//!
//! Motivating bug: asked to "say hi to the design team on Slack", the agent
//! would notice the user's own earlier "hi" messages already in the channel and
//! conclude the task was already done (or stall), instead of posting the new
//! greeting. The rule below makes the intended behaviour explicit -- a request
//! is an instruction to ACT, and pre-existing similar content is not completion.
//!
//! Second motivating bug: asked to email someone with the subject "hello", the
//! agent typed "hello" into the recipients field by mistake, then froze instead
//! of recognizing and fixing its own error. `holo serve`
//! (`hcompai/holo-desktop-cli`, using the closed-source `hai-agent-runtime`) is
//! not vendored source this daemon can edit -- the per-turn guidance block below
//! is the reachable lever for this class of bug, same mechanism as the first.

/// The task-execution framing block, prepended to every turn's prompt. A
/// `&'static str` (not built per call) since it is constant and unconditional.
pub fn task_framing_block() -> &'static str {
    "TASK EXECUTION (how to carry out the user's request):\n\
     - Do the specific thing the user asked for, in full. A request is an \
     instruction to ACT, not merely to check whether it might already be done.\n\
     - Pre-existing similar content is NOT completion. If you are asked to send, \
     post, or write something (for example \"say hi to the design team on \
     Slack\") and you see an earlier or similar message already there -- \
     including ones the user sent themselves -- that does NOT mean the task is \
     finished. Perform the new action the user requested.\n\
     - Only skip or adapt the action if the user explicitly said to (e.g. \
     \"only if it isn't already there\"). When it is genuinely unclear whether \
     duplicating is wanted, prefer completing the requested action; ask only if \
     truly ambiguous.\n\
     - You SHARE this Mac with the user. You may be automatically paused mid-task \
     the moment they start using the mouse or keyboard, and resumed when they go \
     idle. If a turn tells you it is resuming after such an interruption, look at \
     the current on-screen state and CONTINUE from where you left off -- do not \
     restart the task or repeat steps you already completed. Avoid stealing the \
     user's frontmost window when you don't need it.\n\
     - SELF-CORRECTION: after every action, check whether the on-screen result \
     actually matches what you intended -- text landed in the wrong field, the \
     wrong element got clicked, an unexpected dialog or state appeared. If it \
     did not go as intended, do NOT freeze, do NOT restart the whole task, and \
     do NOT ask the user for something you can just fix yourself. Undo or clear \
     the specific wrong step (e.g. clear the wrong field, close the wrong \
     dialog, click the correct target instead), then continue from there. A \
     mistake in one step is a one-step fix, not a reason to stall or reset \
     progress. Only ask the user if a genuine correction attempt fails or the \
     situation is truly ambiguous."
}

/// A short, stable substring of [`task_framing_block`] that witnesses (in a
/// probe or the run_prompt assembly) that the guidance is actually present in a
/// composed prompt -- kept here so the witness and the text share one source.
#[allow(dead_code)] // used by examples/task_framing_probe.rs, not the bin target
pub const TASK_FRAMING_MARKER: &str = "Pre-existing similar content is NOT completion";

/// A short, stable substring witnessing the self-correction rule specifically
/// (distinct from [`TASK_FRAMING_MARKER`] so a probe can assert on this rule in
/// isolation, and so the two motivating bugs each have their own witness anchor).
#[allow(dead_code)] // used by examples/self_correction_probe.rs, not the bin target
pub const SELF_CORRECTION_MARKER: &str = "A mistake in one step is a one-step fix";
