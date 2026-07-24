//! Secure-input watchdog loop: periodically checks whether the Mac's currently-focused field is
//! a secure (password-class) input, and emits [`super::control::HoloControlBridge::emit_secure_input_state`]
//! on CHANGE so the phone can show an honest status instead of an unexplained black rectangle.
//!
//! ## Why this exists
//!
//! Live-reported: the login/lock screen's password field renders solid black in the live-share
//! video. `crate::capture`'s `ScreenCapturer` is backed by ScreenCaptureKit, which deliberately
//! excludes secure UI (the login window's authentication fields, a lock-screen password prompt,
//! `sudo`/Keychain dialogs) from every captured frame -- a WindowServer-level security boundary
//! against screen-recording malware, not a bug in this daemon's capture pipeline, and not
//! bypassable at any privilege level. `crate::permissions::secure_input_active()` reads macOS's
//! own signal for this exact condition (`IsSecureEventInputEnabled()`), so the daemon can tell
//! the phone WHY the video looks the way it does instead of leaving it unexplained.
//!
//! Mirrors `super::stall_watchdog`'s loop shape exactly (same `Arc<HoloBridge>` +
//! `CancellationToken` pattern), but additionally tracks the last-known state itself (rather
//! than delegating to a `HoloControlBridge` method) since the change-detection is the whole
//! point here -- emitting on every tick would spam the phone with a status line every 2s.

use std::sync::Arc;
use std::time::Duration;

use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use super::HoloBridge;

/// How often the watchdog polls `IsSecureEventInputEnabled()`. The underlying call is a cheap,
/// synchronous, side-effect-free C query (no allocation, no IPC round trip beyond the syscall
/// WindowServer itself needs), so a short interval costs nothing meaningful -- short enough that
/// the phone's locked-screen banner appears/disappears within a couple seconds of the real
/// transition, not so short it wakes the runtime needlessly between transitions.
const SECURE_INPUT_TICK_INTERVAL: Duration = Duration::from_secs(2);

/// Runs until `shutdown` is cancelled. On each tick, re-reads
/// [`crate::permissions::secure_input_active`] and emits only when it differs from the
/// PREVIOUS tick's value -- `None` initially so the very first observed state (whichever it is)
/// always emits once, giving a freshly-(re)connected client an authoritative starting value
/// rather than staying silent until the first transition.
pub async fn run_secure_input_watchdog_loop(bridge: Arc<HoloBridge>, shutdown: CancellationToken) {
    let mut interval = tokio::time::interval(SECURE_INPUT_TICK_INTERVAL);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut last: Option<bool> = None;

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                tracing::info!("holo_bridge::secure_input_watchdog: shutdown requested, stopping");
                return;
            }
            _ = interval.tick() => {
                let active = crate::permissions::secure_input_active();
                if last != Some(active) {
                    tracing::info!(active, "holo_bridge::secure_input_watchdog: secure input state changed");
                    bridge.control.emit_secure_input_state(active);
                    last = Some(active);
                }
            }
        }
    }
}
