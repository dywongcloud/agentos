//! Pure-logic CI witness for the stall-watchdog decision core
//! (`TaskFsm::should_nudge`/`mark_nudged`, see `crate::task_fsm` +
//! `crate::holo_bridge::stall_watchdog`'s module docs). Deterministic, no daemon/backend/
//! network needed -- exercises exactly the timing/cooldown logic that decides whether the
//! daemon autonomously nudges a stuck agent turn, without inducing a real (unsafe,
//! unpredictable) live stall against `holo serve`.
//!
//! Run with `cargo run --example stall_watchdog_probe -p holoiroh-daemon`.

use holoiroh_daemon::router::Tier;
use holoiroh_daemon::task_fsm::TaskFsm;

const WINDOW: u64 = 45_000; // matches control.rs's STALL_WATCHDOG_WINDOW
const COOLDOWN: u64 = 60_000; // matches control.rs's STALL_WATCHDOG_NUDGE_COOLDOWN

fn main() {
    let base = 1_000_000_u64;
    let mut fsm = TaskFsm::new("stall-watchdog-probe");
    // `TaskFsm::new` stamps real wall-clock `updated_at_ms`; force it to a known base so this
    // probe is deterministic regardless of when it runs.
    fsm.updated_at_ms = base;

    // Freshly-started task, well within the window: never nudge.
    assert!(
        !fsm.should_nudge(base + 1_000, WINDOW, COOLDOWN),
        "a task 1s old must not be nudged"
    );
    assert!(
        !fsm.should_nudge(base + WINDOW - 1, WINDOW, COOLDOWN),
        "a task 1ms short of the window must not be nudged"
    );

    // Exactly at, and past, the stall window: should nudge.
    assert!(
        fsm.should_nudge(base + WINDOW, WINDOW, COOLDOWN),
        "a task exactly at the stall window must be nudged"
    );
    assert!(
        fsm.should_nudge(base + WINDOW + 10_000, WINDOW, COOLDOWN),
        "a task well past the stall window must be nudged"
    );

    // Mark nudged; immediately after, must NOT nudge again (cooldown).
    let nudge_at = base + WINDOW + 10_000;
    fsm.mark_nudged(nudge_at);
    assert!(
        !fsm.should_nudge(nudge_at + 1_000, WINDOW, COOLDOWN),
        "must not nudge again inside the cooldown"
    );
    assert!(
        !fsm.should_nudge(nudge_at + COOLDOWN - 1, WINDOW, COOLDOWN),
        "must not nudge 1ms short of the cooldown elapsing"
    );

    // Cooldown elapsed and the task is STILL stalled (updated_at_ms never advanced): eligible
    // for a second nudge -- a still-stuck task keeps getting backstopped, not abandoned after one try.
    assert!(
        fsm.should_nudge(nudge_at + COOLDOWN, WINDOW, COOLDOWN),
        "a still-stalled task must be nudge-eligible again once the cooldown elapses"
    );

    // Real progress (observe_working advances the phase) resets the staleness clock:
    // immediately after, must NOT nudge even though the earlier `last_nudge_ms` is old.
    let mut progressing = TaskFsm::new("stall-watchdog-probe-progress");
    progressing.updated_at_ms = base;
    progressing.mark_nudged(base); // pretend it was nudged once, long ago
    let progress_event = serde_json::json!({"kind": "tool_result"});
    let changed = progressing.observe_working(Some(&progress_event));
    assert!(changed, "a real tool_result must advance the phase (Plan -> Execute)");
    assert!(
        !progressing.should_nudge(progressing.updated_at_ms + 1, WINDOW, COOLDOWN),
        "real progress must reset the stall clock -- no nudge right after"
    );

    // Terminal phases are never nudged, no matter how stale.
    let mut terminal = TaskFsm::new("stall-watchdog-probe-terminal");
    terminal.updated_at_ms = base;
    terminal.fail();
    assert!(
        !terminal.should_nudge(base + WINDOW * 10, WINDOW, COOLDOWN),
        "a terminal (failed/done) task must never be nudged"
    );

    // Escalation target: `HoloBridge::force_tier` (called right before the watchdog's
    // redirect-nudge, see control.rs) always requests `Tier::Complex` -- confirm that resolves
    // to the real, verified-live H Company hosted model id (router.rs's own doc: verified
    // against the hosted models gateway 2026-07-20), not a stale/placeholder string.
    assert_eq!(
        Tier::Complex.model_id(),
        "holo3-122b-a10b",
        "watchdog escalation must target the real verified complex-tier model id"
    );

    // The daemon-status marker + force-tier-before-nudge wiring (transparency + escalation)
    // are compile-time constants/glue verified by `cargo build` and code review -- this probe
    // covers the actual decision core they gate on.
    println!(
        "stall_watchdog_probe: OK -- should_nudge/mark_nudged correctly gate on window, cooldown, \
         real progress, and terminal state."
    );
}
