//! Health-check loop: periodically verifies the supervised `holo serve` subprocess (owned by
//! [`crate::holo_bridge::HoloBridge`]) is still alive, and restarts it on crash.
//!
//! ## Why this exists on top of `process::wait_for_health`
//!
//! `HoloServeProcess::spawn` already waits for `holo serve` to become healthy once, at
//! startup. That check does not run again afterward -- if `holo serve` (or the
//! `hai-agent-runtime` process it manages) crashes partway through a session, nothing
//! currently notices. This module is the ongoing supervisor: it polls
//! [`HoloBridge::try_wait_process`] on an interval, and on detecting the process has exited,
//! calls [`HoloBridge::restart_process`] to bring it back.
//!
//! ## Why the iroh P2P session survives a restart
//!
//! `HoloBridge` holds only the `holo serve` child process (behind an interior `Mutex`) and
//! the A2A control bridge built on top of it -- it has no field referencing the
//! `iroh_live::Live` session or the `crate::control_channel::ControlChannel` (both owned
//! separately in `main.rs`, which holds `HoloBridge` itself behind an `Arc` shared with the
//! control channel). [`HoloBridge::restart_process`] mutates only `HoloBridge`'s own interior
//! state; there is nothing on the type that could reach into, let alone tear down, the P2P
//! broadcast or control-channel connection. This is a structural guarantee (the type just
//! doesn't hold that reference), not a behavioral promise that has to be maintained by
//! careful coding at each call site.
//!
//! ## Status reporting
//!
//! Every detected crash and every restart attempt (success or failure) is reported via
//! [`HoloBridge::control`]'s [`HoloControlBridge::emit_daemon_status`] -- the same `emit` path
//! (and therefore the same live control-channel connection, once `crate::control_channel`
//! has one mounted) every A2A-derived [`ControlEvent`] already flows through. This is
//! deliberately the same channel, not a new one: `main.rs` already wires
//! `HoloBridge::replace_event_sink` to point this at the currently-connected peer, so a
//! second parallel channel would just be a second thing that wiring has to duplicate for no
//! benefit.

use std::sync::Arc;
use std::time::Duration;

use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use super::HoloBridge;

/// How often the health-check loop polls `holo serve` liveness.
const HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// Crash-loop guard: if `holo serve` crashes again within this long of the previous restart,
/// back off before retrying instead of respawning at full speed forever (e.g. a persistently
/// broken install, missing dependency, or repeatedly-revoked permission).
const CRASH_LOOP_WINDOW: Duration = Duration::from_secs(30);
const CRASH_LOOP_BACKOFF: Duration = Duration::from_secs(15);

/// Runs until `shutdown` is cancelled. On each tick, checks whether `bridge`'s supervised
/// `holo serve` child has exited; if so, restarts it (with crash-loop backoff) and reports a
/// `DaemonStatus` control event describing what happened either way.
///
/// Takes `Arc<HoloBridge>` -- the same handle `main.rs` shares with
/// `crate::control_channel::ControlChannel` -- rather than an owned `HoloBridge`, since both
/// need concurrent access to the same bridge for the daemon's lifetime.
pub async fn run_health_check_loop(bridge: Arc<HoloBridge>, shutdown: CancellationToken) {
    let mut interval = tokio::time::interval(HEALTH_CHECK_INTERVAL);
    // Ticks are liveness polls, not a queue to drain -- if a tick is missed (e.g. the runtime
    // was briefly starved of CPU), skip the backlog rather than firing a burst of catch-up
    // checks immediately after.
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

    let mut last_restart_at: Option<tokio::time::Instant> = None;

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                tracing::info!("holo_bridge::health: shutdown requested, stopping health-check loop");
                return;
            }
            _ = interval.tick() => {
                check_and_restart_if_needed(&bridge, &mut last_restart_at).await;
            }
        }
    }
}

async fn check_and_restart_if_needed(
    bridge: &Arc<HoloBridge>,
    last_restart_at: &mut Option<tokio::time::Instant>,
) {
    let detail = match bridge.try_wait_process().await {
        Ok(None) => return, // still running -- nothing to do this tick
        Ok(Some(status)) => status.to_string(),
        Err(err) => {
            // try_wait() itself failing is unusual (an OS-level error reaping the process)
            // but must not be treated as "still alive" -- fall through to the restart path
            // defensively rather than silently leaving a possibly-dead process unsupervised.
            tracing::error!("holo_bridge::health: try_wait() failed: {err}, attempting restart");
            format!("health check itself failed: {err}")
        }
    };

    tracing::warn!("holo_bridge::health: holo serve is no longer running ({detail}), restarting");
    bridge.control.emit_daemon_status(format!(
        "Holo bridge (holo serve) stopped unexpectedly ({detail}); restarting..."
    ));

    if let Some(last) = *last_restart_at {
        if last.elapsed() < CRASH_LOOP_WINDOW {
            tracing::warn!(
                "holo_bridge::health: crash loop detected (restarted less than {CRASH_LOOP_WINDOW:?} ago), backing off for {CRASH_LOOP_BACKOFF:?}"
            );
            tokio::time::sleep(CRASH_LOOP_BACKOFF).await;
        }
    }

    match bridge.restart_process().await {
        Ok(()) => {
            *last_restart_at = Some(tokio::time::Instant::now());
            tracing::info!("holo_bridge::health: holo serve restarted successfully");
            bridge
                .control
                .emit_daemon_status("Holo bridge (holo serve) restarted successfully.");
        }
        Err(err) => {
            tracing::error!("holo_bridge::health: restart failed: {err}");
            bridge
                .control
                .emit_daemon_status(format!("Holo bridge (holo serve) restart failed: {err}. Will retry."));
            // Deliberately does not propagate a fatal error or exit the loop -- the daemon
            // (and the iroh P2P session it owns, which this module cannot reach at all -- see
            // module doc) stays up even while holo serve is persistently broken. The next
            // tick will try again.
        }
    }
}

