//! Remote-control input injection: turns the user's iOS touch gestures (moves,
//! clicks, drags, scrolls, typed text) into real `CGEvent`s on the Mac, so the
//! user can escalate and drive the computer directly from the live-share view
//! (see the `RemoteControl` control-channel path in `crate::holo_bridge::control`).
//!
//! Normalized coordinates (`0.0..=1.0` within the captured display) arrive over
//! the wire and are mapped to real global display points here, so the phone
//! never needs to know the Mac's resolution.
//!
//! Posting `CGEvent`s requires Accessibility (`AXIsProcessTrusted`), so callers
//! check [`is_permitted`] and surface a one-time grant hint rather than silently
//! doing nothing. All injected events are synthetic (a nonzero source pid), so
//! `crate::user_activity`'s physical-input tap correctly ignores them -- these
//! are the user's REMOTE inputs, not local hardware activity.
//!
//! ## Reaching the login/lock screen's password field
//!
//! [`text`] and [`key`] post through `CGEventTapLocation::HIDEventTap`, the
//! lowest-level system-wide injection point (the same level real hardware
//! keystrokes arrive at) -- the correct mechanism, and nothing in this file
//! gates it to a particular session. Whether the OS actually *delivers* those
//! synthetic events to a focused secure field is a separate question this
//! file cannot answer by inspection alone: `crate::permissions::secure_input_active`
//! reports when the login window, lock screen, or a `sudo`/Keychain prompt has
//! focus, and Apple's own documented purpose for that state (`SecureEventInput`)
//! is specifically to block synthetic keystroke delivery to such fields --
//! the same protection that stops password-harvesting malware would, by
//! design, equally block a legitimate remote-typing feature. This has not
//! been live-tested against the user's own lock screen (deliberately -- a
//! failed synthetic-unlock attempt risks tripping password-attempt lockout on
//! their real account); confirming it one way or the other needs a disposable
//! test machine, or the account owner's explicit, informed consent to try it
//! on their own Mac.

use std::sync::atomic::{AtomicBool, Ordering};

use objc2_core_foundation::{CGPoint, CGRect};
use objc2_core_graphics::{
    CGDisplayBounds, CGEvent, CGEventField, CGEventTapLocation, CGEventType, CGMainDisplayID,
    CGMouseButton, CGScrollEventUnit,
};

/// Tracks whether a mouse button is currently held, so a `Move` while held is
/// emitted as a DRAG (the only way a click-and-drag registers in AppKit).
static LEFT_DOWN: AtomicBool = AtomicBool::new(false);
static RIGHT_DOWN: AtomicBool = AtomicBool::new(false);

/// Whether the daemon may inject input right now (Accessibility granted).
pub fn is_permitted() -> bool {
    crate::permissions::accessibility_granted()
}

/// Releases any mouse button this module still believes is held, at wherever
/// the cursor currently is.
///
/// `LEFT_DOWN`/`RIGHT_DOWN` were only ever cleared as a side effect of
/// [`button`] receiving a matching up, or of [`click`]. Nothing cleared them
/// when the *client* went away mid-drag — a phone that loses its connection,
/// gets swiped away, or lifts a finger over the letterbox. The Mac was then left
/// inside a live drag/selection session, unattended, with no touch anywhere to
/// end it: the pointer keeps dragging whatever it crosses until someone
/// physically uses the machine.
///
/// Safe to call unconditionally and repeatedly — a synthetic button-up with
/// nothing held is a no-op as far as AppKit is concerned, and this only posts
/// for flags it actually observes set.
pub fn release_all() {
    let p = cursor_location().unwrap_or(CGPoint { x: 0.0, y: 0.0 });
    if LEFT_DOWN.swap(false, Ordering::Relaxed) {
        tracing::info!("releasing a left mouse button still held by remote control");
        if let Some(ev) =
            CGEvent::new_mouse_event(None, CGEventType::LeftMouseUp, p, CGMouseButton::Left)
        {
            post(&ev);
        }
    }
    if RIGHT_DOWN.swap(false, Ordering::Relaxed) {
        tracing::info!("releasing a right mouse button still held by remote control");
        if let Some(ev) =
            CGEvent::new_mouse_event(None, CGEventType::RightMouseUp, p, CGMouseButton::Right)
        {
            post(&ev);
        }
    }
}

/// Map a normalized point (`0..=1` within the captured display) to a global CG
/// point. Uses the PRIMARY display (the daemon captures primary by default -- a
/// captured non-primary display is a documented refinement, not wired here).
pub fn map_normalized(nx: f64, ny: f64) -> CGPoint {
    let bounds = cached_display_bounds();
    let cx = nx.clamp(0.0, 1.0);
    let cy = ny.clamp(0.0, 1.0);
    CGPoint {
        x: bounds.origin.x + cx * bounds.size.width,
        y: bounds.origin.y + cy * bounds.size.height,
    }
}

/// How long a cached display geometry is trusted. Long enough that a drag never pays for the
/// lookup twice, short enough that a resolution or display change corrects itself before the
/// user could act on a misplaced cursor.
const DISPLAY_BOUNDS_TTL: std::time::Duration = std::time::Duration::from_millis(500);

static DISPLAY_BOUNDS: std::sync::Mutex<Option<(std::time::Instant, CGRect)>> =
    std::sync::Mutex::new(None);

/// `CGDisplayBounds(CGMainDisplayID())` is a system call, and a drag makes it 120 times a second
/// on the read loop's inline path, where every microsecond delays reading the next control
/// message. Measured at p50 20us but with 5ms outliers -- a third of a display frame of jitter
/// landing directly in the cursor path -- against 167ns for a key event, which is the same code
/// without the lookup. The geometry it returns changes only when the user changes displays.
fn cached_display_bounds() -> CGRect {
    let now = std::time::Instant::now();
    let mut cached = DISPLAY_BOUNDS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((fetched_at, bounds)) = *cached {
        if now.duration_since(fetched_at) < DISPLAY_BOUNDS_TTL {
            return bounds;
        }
    }
    let bounds = CGDisplayBounds(CGMainDisplayID());
    *cached = Some((now, bounds));
    bounds
}

static APPLIED_MOVES: std::sync::Mutex<Vec<(f64, f64)>> = std::sync::Mutex::new(Vec::new());

fn injection_is_dry_run() -> bool {
    static DRY_RUN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DRY_RUN.get_or_init(|| std::env::var("HOLOIROH_INPUT_DRY_RUN").as_deref() == Ok("1"))
}

pub fn take_applied_moves() -> Vec<(f64, f64)> {
    std::mem::take(
        &mut *APPLIED_MOVES
            .lock()
            .unwrap_or_else(|e| e.into_inner()),
    )
}

static APPLIED_CLICK_STATES: std::sync::Mutex<Vec<i64>> = std::sync::Mutex::new(Vec::new());

pub fn take_applied_click_states() -> Vec<i64> {
    std::mem::take(
        &mut *APPLIED_CLICK_STATES
            .lock()
            .unwrap_or_else(|e| e.into_inner()),
    )
}

fn record_applied_click(states: &[i64]) {
    if !injection_is_dry_run() {
        return;
    }
    APPLIED_CLICK_STATES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .extend_from_slice(states);
}

fn record_applied_move(nx: f64, ny: f64) {
    if !injection_is_dry_run() {
        return;
    }
    APPLIED_MOVES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((nx, ny));
}

fn post(event: &CGEvent) {
    if injection_is_dry_run() {
        return;
    }
    CGEvent::post(CGEventTapLocation::HIDEventTap, Some(event));
}

/// Current cursor location in global CG points (for witnesses / diagnostics).
pub fn cursor_location() -> Option<CGPoint> {
    let ev = CGEvent::new(None)?;
    Some(CGEvent::location(Some(&ev)))
}

/// Move the cursor to the normalized point (a drag if a button is held).
pub fn move_cursor(nx: f64, ny: f64) {
    record_applied_move(nx, ny);
    let p = map_normalized(nx, ny);
    let (ty, btn) = if LEFT_DOWN.load(Ordering::Relaxed) {
        (CGEventType::LeftMouseDragged, CGMouseButton::Left)
    } else if RIGHT_DOWN.load(Ordering::Relaxed) {
        (CGEventType::RightMouseDragged, CGMouseButton::Right)
    } else {
        (CGEventType::MouseMoved, CGMouseButton::Left)
    };
    if let Some(ev) = CGEvent::new_mouse_event(None, ty, p, btn) {
        post(&ev);
    }
}

/// Press (`down: true`) or release a mouse button at the normalized point.
pub fn button(nx: f64, ny: f64, right: bool, down: bool) {
    let p = map_normalized(nx, ny);
    let (ty, cgbtn) = match (right, down) {
        (false, true) => (CGEventType::LeftMouseDown, CGMouseButton::Left),
        (false, false) => (CGEventType::LeftMouseUp, CGMouseButton::Left),
        (true, true) => (CGEventType::RightMouseDown, CGMouseButton::Right),
        (true, false) => (CGEventType::RightMouseUp, CGMouseButton::Right),
    };
    if right {
        RIGHT_DOWN.store(down, Ordering::Relaxed);
    } else {
        LEFT_DOWN.store(down, Ordering::Relaxed);
    }
    if let Some(ev) = CGEvent::new_mouse_event(None, ty, p, cgbtn) {
        post(&ev);
    }
}

/// How close together in time two clicks must be to form a double-click. macOS's own default
/// for `com.apple.mouse.doubleClickThreshold`; deliberately not read from the user's prefs,
/// since doing that from the injection path means spawning a process mid-click.
const DOUBLE_CLICK_WINDOW: std::time::Duration = std::time::Duration::from_millis(500);

/// How far the pointer may drift between two clicks and still count as the same spot. Generous
/// because the phone maps a whole desktop onto a few hundred points, so one touch-point of
/// wobble is several desktop pixels.
const DOUBLE_CLICK_SLOP: f64 = 6.0;

static LAST_CLICK: std::sync::Mutex<Option<(std::time::Instant, CGPoint, bool, i64)>> =
    std::sync::Mutex::new(None);

/// The click state (1 = single, 2 = double, 3 = triple) this click should carry, derived the
/// way the window server derives it for real hardware: from how soon and how near the previous
/// click was.
///
/// The phone cannot send this itself without waiting out the double-click window on EVERY tap
/// before it knows whether a second one is coming -- half a second of dead time on the most
/// common interaction, in a feature whose whole point is feeling immediate. Deriving it here
/// costs nothing and makes a fast double-tap open a folder, as it would with a real mouse.
fn next_click_state(p: CGPoint, right: bool) -> i64 {
    let now = std::time::Instant::now();
    let mut last = LAST_CLICK.lock().unwrap_or_else(|e| e.into_inner());
    let state = match *last {
        Some((when, where_, was_right, previous))
            if was_right == right
                && now.duration_since(when) <= DOUBLE_CLICK_WINDOW
                && (where_.x - p.x).abs() <= DOUBLE_CLICK_SLOP
                && (where_.y - p.y).abs() <= DOUBLE_CLICK_SLOP =>
        {
            (previous + 1).min(3)
        }
        _ => 1,
    };
    *last = Some((now, p, right, state));
    state
}

/// A full click (down+up) at the point. `count > 1` forces a multi-click; `count == 1` lets
/// [`next_click_state`] decide, so consecutive taps become a real double-click.
pub fn click(nx: f64, ny: f64, right: bool, count: u32) {
    let count = count.max(1);
    let p = map_normalized(nx, ny);
    let cgbtn = if right { CGMouseButton::Right } else { CGMouseButton::Left };
    let (dty, uty) = if right {
        (CGEventType::RightMouseDown, CGEventType::RightMouseUp)
    } else {
        (CGEventType::LeftMouseDown, CGEventType::LeftMouseUp)
    };
    let states: Vec<i64> = if count > 1 {
        (1..=count as i64).collect()
    } else {
        vec![next_click_state(p, right)]
    };
    record_applied_click(&states);
    for state in states {
        if let Some(down) = CGEvent::new_mouse_event(None, dty, p, cgbtn) {
            CGEvent::set_integer_value_field(Some(&down), CGEventField::MouseEventClickState, state);
            post(&down);
        }
        if let Some(up) = CGEvent::new_mouse_event(None, uty, p, cgbtn) {
            CGEvent::set_integer_value_field(Some(&up), CGEventField::MouseEventClickState, state);
            post(&up);
        }
    }
    // A click is not a drag; make sure no stale held-button state lingers.
    LEFT_DOWN.store(false, Ordering::Relaxed);
    RIGHT_DOWN.store(false, Ordering::Relaxed);
}

/// Scroll at the point by wheel deltas (line units; negative `dy` scrolls the
/// content up, matching a natural upward swipe).
pub fn scroll(nx: f64, ny: f64, dx: f64, dy: f64) {
    // Move the cursor to the point first so the scroll targets that spot.
    let p = map_normalized(nx, ny);
    if let Some(mv) = CGEvent::new_mouse_event(None, CGEventType::MouseMoved, p, CGMouseButton::Left)
    {
        post(&mv);
    }
    if let Some(ev) = CGEvent::new_scroll_wheel_event2(
        None,
        CGScrollEventUnit::Line,
        2,
        dy.round() as i32,
        dx.round() as i32,
        0,
    ) {
        post(&ev);
    }
}

/// Type a string of text at the current keyboard focus (verbatim unicode).
pub fn text(s: &str) {
    let utf16: Vec<u16> = s.encode_utf16().collect();
    if utf16.is_empty() {
        return;
    }
    if let Some(ev) = CGEvent::new_keyboard_event(None, 0, true) {
        unsafe {
            CGEvent::keyboard_set_unicode_string(Some(&ev), utf16.len() as u64, utf16.as_ptr());
        }
        post(&ev);
    }
    if let Some(ev) = CGEvent::new_keyboard_event(None, 0, false) {
        unsafe {
            CGEvent::keyboard_set_unicode_string(Some(&ev), utf16.len() as u64, utf16.as_ptr());
        }
        post(&ev);
    }
}

/// Press or release a named special key.
pub fn key(name: &str, down: bool) {
    let Some(code) = keycode(name) else {
        return;
    };
    if let Some(ev) = CGEvent::new_keyboard_event(None, code, down) {
        post(&ev);
    }
}

/// Map a named special key to its macOS virtual keycode. Returns `None` for
/// unknown names (the caller then ignores the key event rather than posting a
/// wrong keystroke).
fn keycode(name: &str) -> Option<u16> {
    Some(match name.to_ascii_lowercase().as_str() {
        "return" | "enter" => 36,
        "delete" | "backspace" => 51,
        "escape" | "esc" => 53,
        "tab" => 48,
        "space" => 49,
        "left" => 123,
        "right" => 124,
        "down" => 125,
        "up" => 126,
        _ => return None,
    })
}
