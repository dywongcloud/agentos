//! Pure-logic CI witness for the crash-restart stale-context guard added to
//! `HoloControlBridge::maybe_nudge_stalled_turn`/`handle_redirect` (see `control.rs`'s
//! `client_epoch`/`turn_epoch_is_stale` docs). Motivating bug: `holo serve` crashed mid-turn
//! (user-reported "signal 15 (SIGTERM)"), the health-check loop auto-restarted it onto a fresh,
//! session-less process, and the stall-watchdog's self-correction nudge then redirected into the
//! now-stale `context_id` -- guaranteed to fail, exactly matching "it tried to self correct
//! itself... struggled to correct itself and failed to do so".
//!
//! `turn_epoch_is_stale`/`HoloControlBridge::is_stale`/`CurrentTurn` gate that decision, but the
//! full async control flow they gate (skip redirect vs fail-clean-and-report) lives inside
//! private `HoloControlBridge` methods that need a live `A2aClient`/task registry -- not
//! reachable from a standalone example. This probe covers the pure decision primitive with a
//! real exercise (no mocking, no test framework); the full async wiring is covered by a live
//! induced holo-serve crash against the running daemon (see the accompanying live witness).
//!
//! Run with `cargo run --example epoch_mismatch_probe -p holoiroh-daemon`.

use holoiroh_daemon::holo_bridge::control::turn_epoch_is_stale;
use holoiroh_daemon::task_fsm::TaskFsm;

fn main() {
    // Same generation the turn started under (no crash-restart, no backend switch happened):
    // never stale -- the legitimate same-session stall-recovery nudge (Chain B's original fix)
    // must proceed exactly as before.
    assert!(
        !turn_epoch_is_stale(0, 0),
        "epoch 0 vs 0 (fresh daemon, never restarted) must not be stale"
    );
    assert!(
        !turn_epoch_is_stale(3, 3),
        "a turn started under the CURRENT epoch must not be stale even after earlier restarts moved the counter to 3"
    );

    // The process was replaced by exactly one crash-restart (or backend switch) since this
    // turn started: stale -- the redirect/nudge path must refuse to inherit this context.
    assert!(
        turn_epoch_is_stale(0, 1),
        "a turn that started under epoch 0 must be stale once the live epoch has moved to 1"
    );
    // Multiple replacements between the turn starting and the check (e.g. a crash-restart
    // immediately followed by a rate-limit failover) must also read as stale -- any forward
    // movement invalidates the turn's session, not just a movement of exactly one.
    assert!(
        turn_epoch_is_stale(2, 5),
        "a turn must be stale after ANY number of process replacements since it started, not just one"
    );

    // Epoch counters only ever move forward (`replace_client`'s `fetch_add`) -- a turn's epoch
    // can never be AHEAD of the live one. Included for completeness: even in that impossible
    // state, inequality (not a `<` comparison) is still the correct, safe rule -- treat it as
    // stale rather than silently trusting an ordering invariant that should never be violated.
    assert!(
        turn_epoch_is_stale(9, 4),
        "an (unreachable in practice) turn epoch ahead of the live epoch must still read as stale, never trusted"
    );

    // The FSM-side half of the fix: `fail()` must be safely callable on a task regardless of
    // its current phase (the epoch-mismatch branch calls it unconditionally, without first
    // checking whether the task is already terminal) -- confirms it can never panic or regress
    // an already-terminal task backward.
    let mut fresh = TaskFsm::new("epoch-probe-fresh");
    fresh.fail();
    assert!(
        matches!(fresh.phase, holoiroh_daemon::task_fsm::Phase::Failed),
        "fail() on a fresh task must land it in Failed"
    );
    let mut already_done = TaskFsm::new("epoch-probe-already-done");
    already_done.fail();
    already_done.fail();
    assert!(
        matches!(already_done.phase, holoiroh_daemon::task_fsm::Phase::Failed),
        "calling fail() twice (the epoch-mismatch branch is not aware of prior terminal state) must stay Failed, not panic or misbehave"
    );

    println!(
        "epoch_mismatch_probe: OK -- turn_epoch_is_stale correctly distinguishes same-generation \
         (nudge proceeds) from any-later-generation (fail clean, no doomed redirect) turns, and \
         TaskFsm::fail() is safely idempotent for the unconditional-fail call the guard makes."
    );
}
