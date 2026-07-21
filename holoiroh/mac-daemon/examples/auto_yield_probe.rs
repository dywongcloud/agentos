//! Pure-logic CI witness for cooperative auto-yield's decision function
//! (`crate::auto_yield::decide`) -- the state machine that pauses the agent when
//! the user is active and resumes it when they go idle. Deterministic, no
//! daemon / tap / permission needed: it drives `decide` with injected states and
//! asserts every transition, so the anti-collision behavior is pinned by an
//! executable witness.
//!
//! Run with `cargo run --example auto_yield_probe -p holoiroh-daemon`.

use std::time::Duration;

use holoiroh_daemon::auto_yield::{decide, AutoYieldConfig, YieldAction};

fn main() {
    let cfg = AutoYieldConfig {
        enabled: true,
        activity_secs: 1.5,
        resume_secs: 5.0,
        poll: Duration::from_millis(400),
    };

    // Running + user just acted (idle < activity) -> step aside.
    assert_eq!(
        decide(&cfg, true, false, false, Some(0.3)),
        YieldAction::Pause,
        "user active while running should pause"
    );
    // Away user (idle long) while running -> let the agent work freely.
    assert_eq!(
        decide(&cfg, true, false, false, Some(10.0)),
        YieldAction::None,
        "agent should run freely while the user is away"
    );
    // Already auto-yielded + user still active -> nothing (never double-park).
    assert_eq!(
        decide(&cfg, false, true, false, Some(0.3)),
        YieldAction::None,
        "no action while user still active"
    );
    // Auto-yielded + user sustained-idle past resume threshold -> resume.
    assert_eq!(
        decide(&cfg, false, true, false, Some(6.0)),
        YieldAction::Resume,
        "sustained idle should resume"
    );
    // Auto-yielded but idle only between activity and resume (hysteresis band)
    // -> hold, do not thrash back to running.
    assert_eq!(
        decide(&cfg, false, true, false, Some(3.0)),
        YieldAction::None,
        "hysteresis: not idle long enough to resume yet"
    );
    // A user-initiated pause is NEVER auto-resumed, however idle they are.
    assert_eq!(
        decide(&cfg, false, false, true, Some(600.0)),
        YieldAction::None,
        "auto-yield must never resume a user pause"
    );
    // Tap unavailable (no permission) -> never act (can't tell user from agent).
    assert_eq!(decide(&cfg, true, false, false, None), YieldAction::None);
    assert_eq!(decide(&cfg, false, true, false, None), YieldAction::None);
    // Disabled by config -> never act.
    let off = AutoYieldConfig { enabled: false, ..cfg };
    assert_eq!(decide(&off, true, false, false, Some(0.1)), YieldAction::None);

    // Env-derived config keeps the hysteresis invariant (resume > activity),
    // even if someone sets a resume threshold below the activity threshold.
    let env = AutoYieldConfig::from_env();
    assert!(
        env.resume_secs > env.activity_secs,
        "resume_secs must exceed activity_secs for hysteresis (got resume={} activity={})",
        env.resume_secs,
        env.activity_secs
    );

    println!(
        "auto_yield_probe: OK -- pause-on-active, run-when-away, resume-on-sustained-idle, \
         hysteresis, user-pause-protected, tap-unavailable-safe, disabled-noop all witnessed."
    );
}
