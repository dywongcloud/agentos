use holoiroh_daemon::task_fsm::TaskFsm;

const STALL_WINDOW_MS: u64 = 45_000;
const NUDGE_COOLDOWN_MS: u64 = 90_000;

fn main() {
    let now = holoiroh_wire::epoch_millis_now();
    let mut fsm = TaskFsm::new("probe-consent-pause");

    fsm.updated_at_ms = now - 60_000;
    let nudges_a_waiting_turn = fsm.should_nudge(now, STALL_WINDOW_MS, NUDGE_COOLDOWN_MS);
    println!(
        "turn silent 60s because it is parked on a consent prompt -> should_nudge={nudges_a_waiting_turn} \
         (true here is the live bug: force_tier restarts holo serve and kills the stream)"
    );

    fsm.mark_awaiting_user_decision(now);
    let nudges_while_awaiting = fsm.should_nudge(now, STALL_WINDOW_MS, NUDGE_COOLDOWN_MS);
    println!("while the consent prompt is outstanding -> should_nudge={nudges_while_awaiting}");

    let one_second_after_consent = now + 1_000;
    let nudges_right_after_answer =
        fsm.should_nudge(one_second_after_consent, STALL_WINDOW_MS, NUDGE_COOLDOWN_MS);
    println!(
        "1s after the user finally answers -> should_nudge={nudges_right_after_answer} \
         (guards the second-order bug: a stale 60s-old clock would fire instantly on resume)"
    );

    let genuinely_stalled_at = now + STALL_WINDOW_MS + 1_000;
    let nudges_a_real_stall =
        fsm.should_nudge(genuinely_stalled_at, STALL_WINDOW_MS, NUDGE_COOLDOWN_MS);
    println!(
        "46s of REAL silence after the answer -> should_nudge={nudges_a_real_stall} \
         (the watchdog must still work for genuine hangs)"
    );

    assert!(
        nudges_a_waiting_turn,
        "precondition: an un-marked 60s-silent turn must look stalled, else this probe proves nothing"
    );
    assert!(
        !nudges_while_awaiting,
        "REGRESSION: a turn awaiting the user's consent was treated as stalled"
    );
    assert!(
        !nudges_right_after_answer,
        "REGRESSION: the stall clock was not refreshed by the wait, so a resumed turn nudges instantly"
    );
    assert!(
        nudges_a_real_stall,
        "REGRESSION: a genuinely stalled turn is no longer nudged -- the watchdog was disabled, not fixed"
    );

    println!("VERDICT: OK -- a consent pause is a wait, not a stall; genuine stalls still nudge");
}
