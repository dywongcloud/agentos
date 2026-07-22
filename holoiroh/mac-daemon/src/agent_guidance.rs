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
     user's frontmost window when you don't need it."
}

/// A short, stable substring of [`task_framing_block`] that witnesses (in a
/// probe or the run_prompt assembly) that the guidance is actually present in a
/// composed prompt -- kept here so the witness and the text share one source.
#[allow(dead_code)] // used by examples/task_framing_probe.rs, not the bin target
pub const TASK_FRAMING_MARKER: &str = "Pre-existing similar content is NOT completion";
