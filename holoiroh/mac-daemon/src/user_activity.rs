//! Physical user-input activity tracking, for cooperative auto-yield (see
//! `crate::auto_yield`): the agent shares the user's Mac, so the daemon must
//! know when the *human* is actively using the mouse/keyboard in order to step
//! aside (pause) and resume once they go idle.
//!
//! ## Why a CGEventTap, and not an idle timer
//!
//! The obvious primitive -- "seconds since last input" via `ioreg
//! IOHIDSystem HIDIdleTime` or `CGEventSourceSecondsSinceLastEventType` -- is
//! **reset by the agent's own synthetic events**, witnessed this session:
//! posting synthetic `CGEvent`s dropped `HIDIdleTime` 173s->0.04s and the CG
//! per-type idle 30s->0.02s. So no idle timer can tell the user's input apart
//! from the agent's clicks; auto-yield keyed off one would fire on the agent's
//! own actions.
//!
//! A `CGEventTap` can: every event carries `kCGEventSourceUnixProcessID`, which
//! is `0` for real hardware input and the *injecting process's pid* for a
//! software-posted (synthetic) event (witnessed: synthetic mouse-moves tapped
//! with `sourcePID == <our pid>`). So we tap all input events and record the
//! timestamp only for `sourcePID == 0` -- the physical human -- ignoring the
//! agent entirely.
//!
//! The tap needs Accessibility / Input-Monitoring permission; if `tap_create`
//! returns `None` (not granted), this module reports *unavailable* and
//! `crate::auto_yield` disables itself gracefully rather than misbehaving.

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

/// Milliseconds (since `START`) at which the last PHYSICAL input event was seen.
static LAST_INPUT_MS: AtomicU64 = AtomicU64::new(0);
/// True once a tap has been successfully created and is delivering events.
static AVAILABLE: AtomicBool = AtomicBool::new(false);
/// True once `start()` has spawned the tap thread (idempotency guard).
static STARTED: AtomicBool = AtomicBool::new(false);
/// Monotonic base so `LAST_INPUT_MS` is a small, comparable millisecond count.
static START: OnceLock<Instant> = OnceLock::new();
/// Borrowed pointer to the live tap's `CFMachPort` (owned for the process life
/// on the tap thread), so the callback can re-enable the tap if the OS disables
/// it. Null until the tap is created.
static TAP_PORT: AtomicPtr<CFMachPort> = AtomicPtr::new(std::ptr::null_mut());

fn now_ms() -> u64 {
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// The tap callback: fires for every input event. Records a fresh timestamp only
/// for physical input (`kCGEventSourceUnixProcessID == 0`), and re-enables the
/// tap if the system ever disables it.
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

/// The set of input event types we care about, as a `CGEventMask` bitfield
/// (`1 << CGEventType` per type): mouse move/drag/up/down (all buttons),
/// key up/down, modifier changes, and scroll.
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

/// Start the physical-input tap on a dedicated CFRunLoop thread. Idempotent:
/// only the first call spawns the thread. Non-blocking; the thread runs for the
/// life of the process. If the tap cannot be created (no Accessibility /
/// Input-Monitoring permission), the thread exits and [`is_available`] stays
/// `false` so `crate::auto_yield` disables itself.
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

/// Whether the tap is live (permission granted + delivering events).
pub fn is_available() -> bool {
    AVAILABLE.load(Ordering::SeqCst)
}

/// Seconds since the last PHYSICAL user input, or `None` if the tap is
/// unavailable (no permission) -- in which case auto-yield must disable itself
/// rather than guess. A freshly-started monitor reports ~0 until real input,
/// which harmlessly makes the very first poll treat the user as "just active".
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

/// Reads the debug idle override file, if `HOLOIROH_AUTO_YIELD_FORCE_IDLE_FILE`
/// is set and the file parses to a number. Returns `None` in normal operation.
fn forced_idle_override() -> Option<f64> {
    let path = std::env::var("HOLOIROH_AUTO_YIELD_FORCE_IDLE_FILE").ok()?;
    let raw = std::fs::read_to_string(path).ok()?;
    raw.trim().parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0)
}
