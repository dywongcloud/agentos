use holoiroh_daemon::holo_bridge::process::{
    total_spawn_retry_budget, LONGEST_OBSERVED_PORT_HOLD_AFTER_KILL, MAX_SPAWN_ATTEMPTS,
    SPAWN_RETRY_BACKOFF,
};

fn main() {
    let budget = total_spawn_retry_budget();
    println!(
        "spawn retry budget = {} attempts x {}ms = {}s",
        MAX_SPAWN_ATTEMPTS,
        SPAWN_RETRY_BACKOFF.as_millis(),
        budget.as_secs()
    );
    println!(
        "longest port hold actually observed after a kill = {}s",
        LONGEST_OBSERVED_PORT_HOLD_AFTER_KILL.as_secs()
    );

    let previously_shipped_budget = std::time::Duration::from_millis(8 * 1200);
    println!(
        "budget that failed live = {}s (8 attempts x 1200ms, exhausted while the port was still draining)",
        previously_shipped_budget.as_secs()
    );

    assert!(
        budget >= LONGEST_OBSERVED_PORT_HOLD_AFTER_KILL * 2,
        "REGRESSION: the spawn retry budget ({}s) no longer outlasts the observed post-kill port \
         hold ({}s) with headroom -- a restart will give up while TIME_WAIT is still draining and \
         leave the backend down",
        budget.as_secs(),
        LONGEST_OBSERVED_PORT_HOLD_AFTER_KILL.as_secs()
    );
    assert!(
        previously_shipped_budget < LONGEST_OBSERVED_PORT_HOLD_AFTER_KILL,
        "sanity: the old budget must be shorter than the observed hold, else it was never the bug"
    );

    println!("VERDICT: OK -- the retry budget now outlasts the port hold that defeated it live");
}
