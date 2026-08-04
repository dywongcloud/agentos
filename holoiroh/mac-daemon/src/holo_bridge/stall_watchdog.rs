//! Detects stalled desktop-agent turns and requests self-correction.
//!
//! ## Runtime boundary
//!
//! `holo serve` comes from the third-party `hcompai/holo-desktop-cli` project.
//! The closed-source `hai-agent-runtime` backs that executable.
//! The daemon connects to the executable through the Agent-to-Agent (A2A) protocol.
//! This codebase does not vendor the runtime's internal reasoning or retry loop.
//! Therefore, the daemon cannot modify that loop directly.
//!
//! The request concerned the desktop agent's artificial intelligence (AI).
//! The request said:
//!
//! > "fan out workflows/agents/subagents to make the AI ... able to resolve its own mistakes."
//!
//! The daemon implements this behavior as a supervisory layer.
//!
//! ## Supervision behavior
//!
//! This loop is the verifier in a two-turn executor-and-verifier pattern.
//! It watches [`crate::task_fsm::TaskFsm`]'s `updated_at_ms` value for stalled progress.
//! `TaskFsm` updates this value after each real `TaskUpdate::Working` or `TaskUpdate::Answer`.
//! It also updates the value after each terminal signal.
//! After detecting a stall, the daemon cancels the stuck turn.
//! The daemon then sends a self-correction instruction on the same backend session.
//! The `context_id` identifies that session.
//!
//! These cancel-then-continue mechanics match `ClientMessage::Redirect` for a user-initiated redirect.
//! [`super::control::HoloControlBridge::maybe_nudge_stalled_turn`] implements the check and nudge.
//! This module provides only the periodic driver.
//! Its loop uses the `Arc<HoloBridge>` and `CancellationToken` pattern from [`super::health`].
//!
//! ## Failure witness
//!
//! A user asked the agent to email someone with the subject `"hello"`.
//! The agent entered `"hello"` in the recipients field.
//! It then stopped instead of detecting and correcting the mistake.
//! [`crate::agent_guidance`] tells the agent how to self-correct.
//! The watchdog handles cases where that guidance alone does not recover progress.
//! It nudges a stalled turn before the user must detect and correct the stall.

use std::sync::Arc;
use std::time::Duration;

use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use super::HoloBridge;

/// Checks the running turn for a stall every 10 seconds.
///
/// The 10-second polling interval is intentionally coarser than `STALL_WATCHDOG_WINDOW` in `control.rs`.
/// The window determines whether the turn is stalled.
/// This interval sets only the polling frequency.
const WATCHDOG_TICK_INTERVAL: Duration = Duration::from_secs(10);

/// Checks for stalled turns until `shutdown` is canceled.
///
/// Each tick calls [`super::control::HoloControlBridge::maybe_nudge_stalled_turn`].
/// The method takes no action when no turn is running.
/// It also takes no action when the running turn is not stalled.
pub async fn run_stall_watchdog_loop(bridge: Arc<HoloBridge>, shutdown: CancellationToken) {
    let mut interval = tokio::time::interval(WATCHDOG_TICK_INTERVAL);
    // Ticks are polls, not a queue to drain -- a missed tick (e.g. the runtime was briefly
    // starved) should skip the backlog, not fire a burst of catch-up checks.
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                tracing::info!("holo_bridge::stall_watchdog: shutdown requested, stopping");
                return;
            }
            _ = interval.tick() => {
                bridge.control.maybe_nudge_stalled_turn().await;
            }
        }
    }
}
