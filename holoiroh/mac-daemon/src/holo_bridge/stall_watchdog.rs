//! Stall-watchdog loop: periodically checks the currently-running desktop-agent turn for
//! genuine no-progress, and autonomously nudges it to self-correct.
//!
//! ## Why this exists
//!
//! `holo serve` (`hcompai/holo-desktop-cli`, backed by the closed-source `hai-agent-runtime`)
//! is a third-party binary this daemon dials over A2A -- its internal reasoning/retry loop is
//! not vendored source this codebase can edit. Requested: "fan out workflows/agents/subagents
//! to make the AI ... able to resolve its own mistakes." The reachable realization, given that
//! boundary, is a supervisory layer THIS daemon owns: this loop is the second "agent" in a
//! two-turn executor+verifier pattern -- it watches [`crate::task_fsm::TaskFsm`]'s own
//! `updated_at_ms` (already tracked for every real `TaskUpdate::Working`/`Answer`/terminal
//! signal) for a stall, and when one is detected, cancels the stuck turn and redispatches a
//! self-correction instruction on the SAME backend session (`context_id`) -- exactly the
//! cancel-then-continue mechanics `ClientMessage::Redirect` already implements for a
//! user-initiated redirect, just triggered autonomously. See
//! [`super::control::HoloControlBridge::maybe_nudge_stalled_turn`] for the actual check +
//! nudge logic; this module is only the periodic driver, mirroring `super::health`'s loop
//! shape exactly (same `Arc<HoloBridge>` + `CancellationToken` pattern).
//!
//! Motivating bug: asked to email someone with subject "hello", the agent typed "hello" into
//! the recipients field, then froze instead of noticing and fixing it. The guidance block
//! (`crate::agent_guidance`) tells the agent HOW to self-correct; this watchdog is the backstop
//! for when telling it isn't enough on its own -- a genuinely stuck turn gets an explicit,
//! daemon-initiated nudge instead of sitting stalled until the user notices and intervenes.

use std::sync::Arc;
use std::time::Duration;

use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use super::HoloBridge;

/// How often the watchdog checks the running turn for a stall. Deliberately coarser than
/// `STALL_WATCHDOG_WINDOW` (see `control.rs`) -- the window is what decides "is this stalled",
/// this interval is just the polling cadence.
const WATCHDOG_TICK_INTERVAL: Duration = Duration::from_secs(10);

/// Runs until `shutdown` is cancelled. On each tick, delegates to
/// [`super::control::HoloControlBridge::maybe_nudge_stalled_turn`], which is itself a cheap
/// no-op when nothing is running or nothing is stalled.
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
