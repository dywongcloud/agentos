//! This module tracks physical user-input activity for cooperative auto-yield
//! (see `crate::auto_yield`). The agent shares the user's Mac with a human.
//! The daemon must know when the human actively uses the mouse or keyboard.
//! The daemon then pauses. The daemon resumes once the human goes idle.
//!
//! ## Why a CGEventTap, and not an idle timer
//!
//! The obvious primitive is "seconds since last input", via `ioreg
//! IOHIDSystem HIDIdleTime` or `CGEventSourceSecondsSinceLastEventType`. The
//! agent's own synthetic events reset this primitive. This session witnessed
//! the reset directly: posting synthetic `CGEvent`s dropped `HIDIdleTime`
//! from 173s to 0.04s. The same synthetic events dropped the CG per-type idle
//! value from 30s to 0.02s. So an idle timer cannot tell the user's input
//! apart from the agent's clicks. Auto-yield keyed off an idle timer fires on
//! the agent's own actions.
//!
//! A `CGEventTap` can tell human input apart from agent input. Every event
//! carries `kCGEventSourceUnixProcessID`. This field is `0` for real hardware
//! input. For a software-posted (synthetic) event, this field holds the
//! injecting process's pid (witnessed: synthetic mouse-moves tapped with
//! `sourcePID == <our pid>`). So this module taps all input events. This
//! module records the timestamp only for `sourcePID == 0`, the physical
//! human. This module ignores the agent entirely.
//!
//! The tap needs Accessibility or Input-Monitoring permission. If
//! `tap_create` returns `None` (permission not granted), this module reports
//! itself as unavailable. `crate::auto_yield` then disables itself
//! gracefully instead of misbehaving.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

use objc2_core_foundation::{kCFRunLoopCommonModes, CFMachPort, CFRunLoop};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventMask, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType,
};

/// This field stores the milliseconds since `START` when the module last saw
/// a physical input event.
static LAST_INPUT_MS: AtomicU64 = AtomicU64::new(0);
/// This flag becomes true once the daemon creates the tap and the tap
/// delivers events.
static AVAILABLE: AtomicBool = AtomicBool::new(false);
/// This flag becomes true once `start()` spawns the tap thread. The flag acts
/// as an idempotency guard for `start()`.
static STARTED: AtomicBool = AtomicBool::new(false);
/// This value is the monotonic base that keeps `LAST_INPUT_MS` a small,
/// comparable millisecond count.
static START: OnceLock<Instant> = OnceLock::new();
/// This is a borrowed pointer to the live tap's `CFMachPort`. The tap thread
/// owns this pointer for the life of the process. The callback uses this
/// pointer to re-enable the tap if the OS disables the tap. This pointer is
/// null until the tap exists.
static TAP_PORT: AtomicPtr<CFMachPort> = AtomicPtr::new(std::ptr::null_mut());

fn now_ms() -> u64 {
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// This is the tap callback. It fires for every input event. It records a
/// fresh timestamp only for physical input (`kCGEventSourceUnixProcessID ==
/// 0`). It re-enables the tap if the system disables the tap.
unsafe extern "C-unwind" fn tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: NonNull<CGEvent>,
    _user_info: *mut c_void,
) -> *mut CGEvent {
    // The OS can disable a tap (timeout, or on certain user input); re-enable via
    // the port we stashed in TAP_PORT so we don't go deaf. (Listen-only taps
    // rarely time out, but this keeps us robust.)
    if event_type == CGEventType::TapDisabledByTimeout
        || event_type == CGEventType::TapDisabledByUserInput
    {
        let port = TAP_PORT.load(Ordering::SeqCst);
        if !port.is_null() {
            CGEvent::tap_enable(unsafe { &*port }, true);
        }
        return event.as_ptr();
    }

    // Physical hardware input has no injecting process (pid 0); a synthetic
    // event posted by the agent carries the agent process's pid. Only the human
    // resets the "last user input" clock.
    let pid = CGEvent::integer_value_field(
        Some(unsafe { event.as_ref() }),
        CGEventField::EventSourceUnixProcessID,
    );
    if pid == 0 {
        LAST_INPUT_MS.store(now_ms(), Ordering::Relaxed);
    }

    // Listen-only tap: pass the event through unchanged.
    event.as_ptr()
}

/// This function returns the input event types this module cares about, as
/// a `CGEventMask` bitfield (one bit per type, `1 << CGEventType`). The
/// types are:
/// - mouse move, drag, up, and down, for all buttons
/// - key up and key down
/// - modifier changes
/// - scroll
fn input_event_mask() -> CGEventMask {
    // Raw CGEventType values (stable ABI constants): left/right/other mouse
    // down(1,3,25)/up(2,4,26)/dragged(6,7,27), mouseMoved(5), keyDown(10),
    // keyUp(11), flagsChanged(12), scrollWheel(22), tabletPointer(23/24).
    let types: [u64; 15] = [1, 2, 3, 4, 5, 6, 7, 10, 11, 12, 22, 23, 24, 25, 26];
    let mut mask: u64 = 0;
    for t in types {
        mask |= 1u64 << t;
    }
    mask as CGEventMask
}

/// Start the physical-input tap on a dedicated `CFRunLoop` thread.
/// This function is idempotent. Only the first call spawns the thread.
/// This function does not block. The thread runs for the life of the process.
/// If the daemon lacks Accessibility or Input-Monitoring permission, the tap
/// cannot be created. In that case, the thread exits. [`is_available`] then
/// stays `false`. `crate::auto_yield` disables itself as a result.
pub fn start() {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    // Seed the clock so "seconds since input" is 0 at startup rather than huge.
    START.get_or_init(Instant::now);
    LAST_INPUT_MS.store(now_ms(), Ordering::Relaxed);

    std::thread::Builder::new()
        .name("holoiroh-user-activity-tap".into())
        .spawn(|| {
            // `port` (a CFRetained<CFMachPort>) is owned by this thread and stays
            // alive for the whole `CFRunLoop::run()` below.
            let port = unsafe {
                CGEvent::tap_create(
                    CGEventTapLocation::SessionEventTap,
                    CGEventTapPlacement::HeadInsertEventTap,
                    CGEventTapOptions::ListenOnly,
                    input_event_mask(),
                    Some(tap_callback),
                    std::ptr::null_mut(),
                )
            };
            let Some(port) = port else {
                tracing::warn!(
                    "user_activity: CGEventTap could not be created (grant this daemon \
                     Input Monitoring / Accessibility permission to enable auto-yield); \
                     auto-yield will be disabled"
                );
                return;
            };
            let port_ref: &CFMachPort = &port;
            // Publish a borrowed pointer so the callback can re-enable the tap on
            // an OS-initiated disable; valid for as long as `port` lives (forever).
            TAP_PORT.store(
                (port_ref as *const CFMachPort) as *mut CFMachPort,
                Ordering::SeqCst,
            );

            let source = CFMachPort::new_run_loop_source(None, Some(port_ref), 0);
            let Some(source) = source else {
                tracing::warn!("user_activity: failed to create run-loop source for tap");
                return;
            };
            let Some(run_loop) = CFRunLoop::current() else {
                tracing::warn!("user_activity: no current run loop on tap thread");
                return;
            };
            let common_modes = unsafe { kCFRunLoopCommonModes };
            run_loop.add_source(Some(&source), common_modes);
            CGEvent::tap_enable(port_ref, true);

            AVAILABLE.store(true, Ordering::SeqCst);
            tracing::info!("user_activity: physical-input CGEventTap live (auto-yield enabled)");

            // Blocks this thread forever, delivering events to `tap_callback`.
            CFRunLoop::run();
        })
        .expect("spawn user-activity tap thread");
}

/// Reports whether the tap is live. The tap is live when the daemon holds
/// permission and delivers events.
pub fn is_available() -> bool {
    AVAILABLE.load(Ordering::SeqCst)
}

/// Returns the seconds since the last PHYSICAL user input.
/// If the tap is unavailable (no permission granted), this function returns
/// `None`. In that case, auto-yield must disable itself instead of guessing.
/// A freshly-started monitor reports ~0 seconds until real input arrives.
/// This harmlessly makes the first poll treat the user as "just active".
pub fn seconds_since_user_input() -> Option<f64> {
    // Test seam: physical input cannot be injected synthetically (that is the
    // whole point of the source-PID classifier), so to witness the auto-yield
    // pause/resume PIPELINE end-to-end without a human at the keyboard, an
    // integration witness can point `HOLOIROH_AUTO_YIELD_FORCE_IDLE_FILE` at a
    // file whose contents are the forced "seconds since user input". Only the
    // idle VALUE (the tap's output) is injected here; the classifier itself is
    // witnessed separately. Absent the env, this never touches the filesystem.
    if let Some(forced) = forced_idle_override() {
        return Some(forced);
    }
    if !AVAILABLE.load(Ordering::SeqCst) {
        return None;
    }
    let last = LAST_INPUT_MS.load(Ordering::Relaxed);
    let now = now_ms();
    Some((now.saturating_sub(last)) as f64 / 1000.0)
}

/// Reads the debug idle override file.
/// This override applies only when `HOLOIROH_AUTO_YIELD_FORCE_IDLE_FILE` is
/// set. The override also requires the file content to parse as a number.
/// This function returns `None` during normal operation.
fn forced_idle_override() -> Option<f64> {
    let path = std::env::var("HOLOIROH_AUTO_YIELD_FORCE_IDLE_FILE").ok()?;
    let raw = std::fs::read_to_string(path).ok()?;
    raw.trim().parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0)
}
