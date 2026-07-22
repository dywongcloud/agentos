//! Cooperative auto-yield: the agent shares the user's Mac, so when the human
//! starts using the mouse/keyboard the daemon steps the agent aside (pauses the
//! running turn) and resumes once the human goes idle. This is the reachable,
//! honest form of "the agent works in its own space without colliding with you"
//! on macOS -- true input isolation is impossible on a single login session
//! (CGEvents share one global focus), and the computer-use backend is a sealed
//! signed app, so the lever we have is *when* the agent is allowed to act.
//!
//! Physical-vs-synthetic input is distinguished by [`crate::user_activity`] (a
//! CGEventTap keyed on `kCGEventSourceUnixProcessID`), so the agent's own clicks
//! never look like the user and never trigger a self-yield.
//!
//! Pause/resume reuse the existing control machinery (`HoloControlBridge`): a
//! pause cancels the backend turn but keeps its A2A `context_id`, so resume
//! continues on the same session (history preserved) rather than blindly
//! re-running -- see `control.rs`'s pause/resume notes.

use std::sync::Arc;
use std::time::Duration;

use crate::holo_bridge::HoloBridge;
use crate::user_activity;

/// What the monitor should do on a given tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YieldAction {
    /// Do nothing this tick.
    None,
    /// The user is active while the agent is running -- step the agent aside.
    Pause,
    /// The user has gone idle and the agent is auto-yielded -- let it resume.
    Resume,
}

/// Tunables for the auto-yield loop.
#[derive(Debug, Clone, Copy)]
pub struct AutoYieldConfig {
    /// Master switch. Default on.
    pub enabled: bool,
    /// The user counts as "active" (yield to them) if their last physical input
    /// was more recent than this many seconds. Small = responsive yielding.
    pub activity_secs: f64,
    /// Resume only after the user has been idle for at least this many seconds.
    /// Larger than `activity_secs` gives hysteresis so brief pauses in the
    /// user's own typing don't thrash the agent between pause and resume.
    pub resume_secs: f64,
    /// How often the monitor samples the state.
    pub poll: Duration,
}

impl Default for AutoYieldConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            activity_secs: 1.5,
            resume_secs: 5.0,
            poll: Duration::from_millis(400),
        }
    }
}

impl AutoYieldConfig {
    /// Read config from the environment, falling back to [`Default`]. Hooks:
    /// `HOLOIROH_AUTO_YIELD` (0/false/no disables), `HOLOIROH_AUTO_YIELD_ACTIVITY_SECS`,
    /// `HOLOIROH_AUTO_YIELD_RESUME_SECS`.
    pub fn from_env() -> Self {
        let d = Self::default();
        let enabled = match std::env::var("HOLOIROH_AUTO_YIELD") {
            Ok(v) => !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "no" | "off"),
            Err(_) => d.enabled,
        };
        let parse_secs = |key: &str, fallback: f64| -> f64 {
            std::env::var(key)
                .ok()
                .and_then(|v| v.trim().parse::<f64>().ok())
                .filter(|v| v.is_finite() && *v >= 0.0)
                .unwrap_or(fallback)
        };
        let activity_secs = parse_secs("HOLOIROH_AUTO_YIELD_ACTIVITY_SECS", d.activity_secs);
        let mut resume_secs = parse_secs("HOLOIROH_AUTO_YIELD_RESUME_SECS", d.resume_secs);
        // Keep the hysteresis invariant: resume threshold must exceed the
        // activity threshold, or the loop could pause and resume on one tick.
        if resume_secs <= activity_secs {
            resume_secs = activity_secs + 1.0;
        }
        Self { enabled, activity_secs, resume_secs, poll: d.poll }
    }
}

/// The pure auto-yield decision -- no I/O, so it is exercised directly by
/// `examples/auto_yield_probe.rs`. `user_idle` is `None` when the input tap is
/// unavailable (no permission), in which case we never act.
///
/// - `busy`: a turn is currently running.
/// - `auto_yielded`: the current pause was created by auto-yield (not the user).
/// - `user_paused`: the user themselves paused (we must never override that).
pub fn decide(
    cfg: &AutoYieldConfig,
    busy: bool,
    auto_yielded: bool,
    user_paused: bool,
    user_idle: Option<f64>,
) -> YieldAction {
    if !cfg.enabled {
        return YieldAction::None;
    }
    let Some(idle) = user_idle else {
        // No physical-input signal -> cannot tell user from agent -> do nothing.
        return YieldAction::None;
    };
    if user_paused {
        // The human deliberately paused; auto-yield never fights that.
        return YieldAction::None;
    }
    if busy && !auto_yielded && idle < cfg.activity_secs {
        return YieldAction::Pause;
    }
    if auto_yielded && idle >= cfg.resume_secs {
        return YieldAction::Resume;
    }
    YieldAction::None
}

/// Spawn the background monitor. Starts the physical-input tap and then samples
/// state on `cfg.poll`, driving [`HoloBridge::auto_yield_pause`] /
/// [`HoloBridge::auto_yield_resume`]. No-op (logs once) if disabled or if the
/// tap never becomes available.
pub fn spawn_monitor(bridge: Arc<HoloBridge>) {
    let cfg = AutoYieldConfig::from_env();
    if !cfg.enabled {
        tracing::info!("auto-yield: disabled by HOLOIROH_AUTO_YIELD");
        return;
    }
    user_activity::start();
    tokio::spawn(async move {
        // Give the tap a moment to come up (permission check, run-loop start).
        tokio::time::sleep(Duration::from_millis(800)).await;
        if !user_activity::is_available() {
            tracing::warn!(
                "auto-yield: physical-input tap unavailable (grant Input Monitoring / \
                 Accessibility to the daemon); auto-yield inactive this run"
            );
            // Keep polling anyway: the grant can be given while running, and the
            // tap flips available without a restart.
        } else {
            tracing::info!(
                activity_secs = cfg.activity_secs,
                resume_secs = cfg.resume_secs,
                "auto-yield: active -- the agent will step aside while you use the Mac"
            );
        }
        loop {
            tokio::time::sleep(cfg.poll).await;
            // Stand down entirely while the user is in hands-on remote control:
            // take-control owns the pause slot then, and the two must not race.
            if bridge.is_remote_control_active() {
                continue;
            }
            let (busy, _queued) = bridge.busy_state();
            let auto_yielded = bridge.is_auto_yielded();
            let paused = bridge.is_paused();
            let user_paused = paused && !auto_yielded;
            let user_idle = user_activity::seconds_since_user_input();
            match decide(&cfg, busy, auto_yielded, user_paused, user_idle) {
                YieldAction::Pause => bridge.auto_yield_pause().await,
                YieldAction::Resume => bridge.auto_yield_resume().await,
                YieldAction::None => {}
            }
        }
    });
}
