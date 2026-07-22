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

use std::sync::atomic::{AtomicBool, Ordering};

use objc2_core_foundation::CGPoint;
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

/// Map a normalized point (`0..=1` within the captured display) to a global CG
/// point. Uses the PRIMARY display (the daemon captures primary by default -- a
/// captured non-primary display is a documented refinement, not wired here).
pub fn map_normalized(nx: f64, ny: f64) -> CGPoint {
    let bounds = CGDisplayBounds(CGMainDisplayID());
    let cx = nx.clamp(0.0, 1.0);
    let cy = ny.clamp(0.0, 1.0);
    CGPoint {
        x: bounds.origin.x + cx * bounds.size.width,
        y: bounds.origin.y + cy * bounds.size.height,
    }
}

fn post(event: &CGEvent) {
    CGEvent::post(CGEventTapLocation::HIDEventTap, Some(event));
}

/// Current cursor location in global CG points (for witnesses / diagnostics).
#[allow(dead_code)] // used by examples/remote_input_probe.rs, not the bin target
pub fn cursor_location() -> Option<CGPoint> {
    let ev = CGEvent::new(None)?;
    Some(CGEvent::location(Some(&ev)))
}

/// Move the cursor to the normalized point (a drag if a button is held).
pub fn move_cursor(nx: f64, ny: f64) {
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

/// A full click (down+up) at the point; `count == 2` is a double-click.
pub fn click(nx: f64, ny: f64, right: bool, count: u32) {
    let count = count.max(1);
    let p = map_normalized(nx, ny);
    let cgbtn = if right { CGMouseButton::Right } else { CGMouseButton::Left };
    let (dty, uty) = if right {
        (CGEventType::RightMouseDown, CGEventType::RightMouseUp)
    } else {
        (CGEventType::LeftMouseDown, CGEventType::LeftMouseUp)
    };
    for i in 1..=count {
        if let Some(down) = CGEvent::new_mouse_event(None, dty, p, cgbtn) {
            CGEvent::set_integer_value_field(
                Some(&down),
                CGEventField::MouseEventClickState,
                i as i64,
            );
            post(&down);
        }
        if let Some(up) = CGEvent::new_mouse_event(None, uty, p, cgbtn) {
            CGEvent::set_integer_value_field(
                Some(&up),
                CGEventField::MouseEventClickState,
                i as i64,
            );
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
