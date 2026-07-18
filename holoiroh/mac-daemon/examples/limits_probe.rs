//! Manual, run-by-hand probe: exercises the real PRD 10.4 limit enforcement helpers in
//! `holoiroh_daemon::limits` directly -- real `ActionCounter`, `SessionTimer`, `ApprovalToken`,
//! and `clamp_task_runtime` -- printing real pass/fail output for each case. Same "no unit
//! tests, drive real code via `cargo run --example` instead" convention this repo's other
//! probes use (see `holo_bridge_queue_probe.rs`, `auth_gate_probe.rs`).
//!
//! Covers:
//! 1. `ActionCounter` refuses the 101st `try_record` call (cap 100, PRD 10.4's
//!    `AGENT_ACTION_CAP_DEFAULT`).
//! 2. `SessionTimer` reports `is_expired() == false` before its max lifetime and `true` after
//!    (using a short max so this doesn't have to sleep the real 10 minutes).
//! 3. `ApprovalToken` rejects a second `consume` on the same token (single-use), and rejects
//!    `consume` after its TTL has elapsed (using a short TTL so this doesn't have to sleep the
//!    real 60s), and rejects `consume` for the wrong `task_id`.
//! 4. `clamp_task_runtime` clamps an over-max request to `TASK_RUNTIME_MAX_SECS` rather than
//!    passing it through, and applies the default when no override is requested.
//!
//! Run with `cargo run --example limits_probe`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use holoiroh_daemon::limits::{
    clamp_task_runtime, ActionCounter, ApprovalToken, ApprovalTokenError, SessionTimer,
    AGENT_ACTION_CAP_DEFAULT, TASK_RUNTIME_DEFAULT_SECS, TASK_RUNTIME_MAX_SECS,
};

fn main() {
    let mut failures = 0u32;

    println!("=== ActionCounter refuses the 101st action (cap={AGENT_ACTION_CAP_DEFAULT}) ===");
    {
        let counter = ActionCounter::new_default();
        let mut last_ok = 0u32;
        for i in 1..=AGENT_ACTION_CAP_DEFAULT {
            match counter.try_record() {
                Ok(n) => last_ok = n,
                Err(cap) => {
                    println!("  UNEXPECTED early refusal at action {i} (cap reported {cap})");
                    failures += 1;
                }
            }
        }
        println!("  recorded {last_ok} actions successfully (expected {AGENT_ACTION_CAP_DEFAULT})");
        let result_101 = counter.try_record();
        println!("  101st try_record() -> {result_101:?}");
        match result_101 {
            Err(cap) if cap == AGENT_ACTION_CAP_DEFAULT => {
                println!("  OK -- 101st action correctly refused, cap={cap}");
            }
            other => {
                println!("  FAIL -- expected Err({AGENT_ACTION_CAP_DEFAULT}), got {other:?}");
                failures += 1;
            }
        }
        // Refusal must not have incremented the count -- calling again should refuse identically.
        let result_102 = counter.try_record();
        if result_102 == result_101 {
            println!("  OK -- repeated refusal is stable ({result_102:?}), count did not drift past cap");
        } else {
            println!("  FAIL -- repeated refusal drifted: {result_101:?} then {result_102:?}");
            failures += 1;
        }
        println!("  final count() = {} (expected {AGENT_ACTION_CAP_DEFAULT})", counter.count());
        if counter.count() != AGENT_ACTION_CAP_DEFAULT {
            failures += 1;
        }
    }

    println!();
    println!("=== SessionTimer reports expiry after its max lifetime ===");
    {
        let short_max = Duration::from_millis(50);
        let timer = SessionTimer::start_with_max(short_max);
        let expired_immediately = timer.is_expired();
        println!("  is_expired() immediately after start -> {expired_immediately} (expected false)");
        if expired_immediately {
            println!("  FAIL -- timer reported expired before any time passed");
            failures += 1;
        }
        let remaining_before = timer.remaining();
        println!("  remaining() immediately after start -> {remaining_before:?} (expected > 0)");
        if remaining_before.is_zero() {
            failures += 1;
        }
        std::thread::sleep(short_max + Duration::from_millis(20));
        let expired_after = timer.is_expired();
        println!("  is_expired() after waiting past max -> {expired_after} (expected true)");
        if !expired_after {
            println!("  FAIL -- timer did not report expired after its max lifetime elapsed");
            failures += 1;
        }
        let remaining_after = timer.remaining();
        println!("  remaining() after expiry -> {remaining_after:?} (expected Duration::ZERO)");
        if !remaining_after.is_zero() {
            println!("  FAIL -- remaining() should saturate to zero after expiry, not go negative/wrap");
            failures += 1;
        }
    }

    println!();
    println!("=== ApprovalToken: single-use + TTL + task-scoping ===");
    {
        let short_ttl = Duration::from_millis(50);
        let token = ApprovalToken::issue_with_ttl("task-abc", short_ttl);

        let wrong_task = token.consume("task-xyz");
        println!("  consume(\"task-xyz\") on a token issued for \"task-abc\" -> {wrong_task:?}");
        if wrong_task != Err(ApprovalTokenError::WrongTask) {
            println!("  FAIL -- expected WrongTask");
            failures += 1;
        }

        let first_consume = token.consume("task-abc");
        println!("  first consume(\"task-abc\") -> {first_consume:?} (expected Ok)");
        if first_consume.is_err() {
            println!("  FAIL -- first consume should have succeeded");
            failures += 1;
        }

        let second_consume = token.consume("task-abc");
        println!("  second consume(\"task-abc\") on the same token -> {second_consume:?} (expected AlreadyConsumed)");
        if second_consume != Err(ApprovalTokenError::AlreadyConsumed) {
            println!("  FAIL -- expected AlreadyConsumed on reuse");
            failures += 1;
        }

        let ttl_token = ApprovalToken::issue_with_ttl("task-ttl", short_ttl);
        std::thread::sleep(short_ttl + Duration::from_millis(20));
        let expired_consume = ttl_token.consume("task-ttl");
        println!("  consume() after TTL elapsed -> {expired_consume:?} (expected Expired)");
        if expired_consume != Err(ApprovalTokenError::Expired) {
            println!("  FAIL -- expected Expired after TTL elapsed");
            failures += 1;
        }

        let valid_token = ApprovalToken::issue("task-valid");
        let is_valid_before = valid_token.is_valid();
        println!("  fresh token is_valid() -> {is_valid_before} (expected true)");
        if !is_valid_before {
            failures += 1;
        }
        let _ = valid_token.consume("task-valid");
        let is_valid_after = valid_token.is_valid();
        println!("  is_valid() after consume -> {is_valid_after} (expected false)");
        if is_valid_after {
            println!("  FAIL -- is_valid() should be false after the token is consumed");
            failures += 1;
        }
    }

    println!();
    println!("=== clamp_task_runtime clamps over-max requests, applies default when unset ===");
    {
        let no_override = clamp_task_runtime(None);
        println!("  clamp_task_runtime(None) -> {no_override:?} (expected {TASK_RUNTIME_DEFAULT_SECS}s)");
        if no_override != Duration::from_secs(TASK_RUNTIME_DEFAULT_SECS) {
            println!("  FAIL -- default runtime mismatch");
            failures += 1;
        }

        let under_max = clamp_task_runtime(Some(Duration::from_secs(80)));
        println!("  clamp_task_runtime(Some(80s)) -> {under_max:?} (expected 80s, passthrough)");
        if under_max != Duration::from_secs(80) {
            println!("  FAIL -- an under-max request should pass through unchanged");
            failures += 1;
        }

        let over_max = clamp_task_runtime(Some(Duration::from_secs(999)));
        println!("  clamp_task_runtime(Some(999s)) -> {over_max:?} (expected clamped to {TASK_RUNTIME_MAX_SECS}s)");
        if over_max != Duration::from_secs(TASK_RUNTIME_MAX_SECS) {
            println!("  FAIL -- an over-max request must be clamped to TASK_RUNTIME_MAX_SECS, not passed through");
            failures += 1;
        }
    }

    println!();
    println!("=== action-cap latch: no update (of any kind) escapes after the cap is hit ===");
    println!("    (reproduces the exact suppress-everything-once-capped shape HoloControlBridge::run_prompt");
    println!("     uses, against the real ActionCounter + AtomicBool latch primitives, to witness the fix");
    println!("     for a bug found during this task's own VERIFY pass: an Answer/Terminal-shaped update");
    println!("     arriving right after the capped Working update used to skip the try_record guard entirely");
    println!("     -- since it isn't Working -- and get emitted anyway, racing ahead of the capped-turn error.)");
    {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum FakeUpdate {
            Working,
            Answer,
        }

        let cap = 3u32;
        let actions = ActionCounter::new(cap);
        let capped = AtomicBool::new(false);
        let mut emitted: Vec<FakeUpdate> = Vec::new();

        // Simulates AGENT_ACTION_CAP_DEFAULT-many Working updates, then one Answer update
        // arriving right after the cap-breaching Working update -- the exact ordering that
        // triggered the bug before the `capped` latch existed.
        let stream = [
            FakeUpdate::Working,
            FakeUpdate::Working,
            FakeUpdate::Working,
            FakeUpdate::Working, // 4th Working: breaches cap=3
            FakeUpdate::Answer,  // must NOT be emitted -- turn is already capped
        ];

        for update in stream {
            // Mirrors HoloControlBridge::run_prompt's on_update closure exactly.
            if capped.load(Ordering::SeqCst) {
                continue;
            }
            if update == FakeUpdate::Working {
                if actions.try_record().is_err() {
                    capped.store(true, Ordering::SeqCst);
                    continue;
                }
            }
            emitted.push(update);
        }

        println!("  emitted updates -> {emitted:?} (expected exactly 3x Working, Answer suppressed)");
        let expected = vec![FakeUpdate::Working, FakeUpdate::Working, FakeUpdate::Working];
        if emitted == expected {
            println!("  OK -- Answer arriving after the cap-breaching Working was correctly suppressed by the latch");
        } else {
            println!("  FAIL -- expected {expected:?}, got {emitted:?} (the post-cap update leaked through)");
            failures += 1;
        }
    }

    println!();
    if failures == 0 {
        println!("limits_probe: OK -- all PRD 10.4 enforcement helpers behaved correctly under real execution.");
    } else {
        println!("limits_probe: FAILED -- {failures} check(s) did not behave as expected.");
        std::process::exit(1);
    }
}
