//! Reports secure-input state changes to the app.
//!
//! The loop polls the Mac's focused field for secure password-class input.
//! After a change, it calls [`super::control::HoloControlBridge::emit_secure_input_state`].
//! The app can then explain a black area in the media stream.
//!
//! ## Capture behavior
//!
//! A live report showed a solid black password field in login-screen and lock-screen video.
//! [`crate::capture::ScreenCapturer`] uses ScreenCaptureKit.
//! ScreenCaptureKit excludes secure user interface (UI) elements from every captured frame.
//! These elements include login-window authentication fields and lock-screen password prompts.
//! They also include `sudo` dialogs and Keychain dialogs.
//! This exclusion is a WindowServer-level security boundary against screen-recording malware.
//! The exclusion is not a defect in the daemon's capture pipeline.
//! No privilege level can bypass this boundary.
//!
//! [`crate::permissions::secure_input_active`] reads `IsSecureEventInputEnabled()` for this condition.
//! The daemon can therefore report why the media stream contains a black area.
//!
//! ## Loop design
//!
//! The loop uses the `Arc<HoloBridge>` and `CancellationToken` pattern from [`super::stall_watchdog`].
//! It tracks the last state instead of delegating state tracking to `HoloControlBridge`.
//! This loop detects changes.
//! Emitting at every 2-second tick would repeatedly add the same app status.

use std::sync::Arc;
use std::time::Duration;

use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use super::HoloBridge;

/// Polls `IsSecureEventInputEnabled()` every 2 seconds.
///
/// The synchronous C call has no side effects.
/// It does not allocate memory.
/// It makes no interprocess communication (IPC) round trip beyond the required WindowServer system call.
/// The app banner updates on the first 2-second poll after a real transition.
/// This interval also avoids unnecessary runtime wakeups between transitions.
const SECURE_INPUT_TICK_INTERVAL: Duration = Duration::from_secs(2);

/// Reports secure-input state changes until `shutdown` is canceled.
///
/// Each tick reads [`crate::permissions::secure_input_active`].
/// The loop emits only when the value differs from the preceding tick.
/// The initial previous value is `None`.
/// Therefore, the loop emits its first observed state once.
/// This first emission gives a newly connected or reconnected app an authoritative starting value.
/// The app does not have to wait for a transition.
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
