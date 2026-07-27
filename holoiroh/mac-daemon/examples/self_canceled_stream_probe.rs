use holoiroh_daemon::holo_bridge::control::TurnsCanceledByUs;

fn main() {
    let canceled = TurnsCanceledByUs::default();

    let genuine_backend_failure = canceled.stream_error_here_was_expected("req-backend-died");
    println!(
        "stream error on a turn we never cancelled -> expected={genuine_backend_failure} \
         (false means a real backend failure still reaches the user)"
    );

    canceled.record("req-watchdog-nudge");
    let after_our_own_cancel = canceled.stream_error_here_was_expected("req-watchdog-nudge");
    println!(
        "stream error right after WE cancelled that turn -> expected={after_our_own_cancel} \
         (true means the self-inflicted teardown is not reported as a failure)"
    );

    let same_id_a_second_time = canceled.stream_error_here_was_expected("req-watchdog-nudge");
    println!(
        "a LATER stream error on that same request id -> expected={same_id_a_second_time} \
         (false means the suppression is one-shot and cannot permanently mute a request id)"
    );

    canceled.record("req-a");
    canceled.record("req-b");
    let unrelated_still_reported = canceled.stream_error_here_was_expected("req-c");
    println!(
        "an unrelated turn while two cancels are outstanding -> expected={unrelated_still_reported}"
    );

    assert!(
        !genuine_backend_failure,
        "REGRESSION: a real backend failure would be swallowed"
    );
    assert!(
        after_our_own_cancel,
        "REGRESSION: our own cancel still surfaces as a user-facing error"
    );
    assert!(
        !same_id_a_second_time,
        "REGRESSION: suppression is sticky -- a later genuine failure on this id would be hidden"
    );
    assert!(
        !unrelated_still_reported,
        "REGRESSION: suppression leaked across request ids"
    );

    println!(
        "VERDICT: OK -- only the specific turn this daemon cancelled is suppressed, exactly once"
    );
}
