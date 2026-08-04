//! Injects remote-control input from the app as synthetic macOS `CGEvent`s.
//!
//! Input includes moves, clicks, drags, scrolls, and typed text from iOS touch gestures.
//! This input lets the user control the Mac directly from the live-share view.
//! See the `RemoteControl` control-channel path in `crate::holo_bridge::control`.
//!
//! The control channel supplies normalized coordinates within `0.0..=1.0` of the captured display.
//! This module maps them to global display points.
//! The app does not need the Mac display resolution.
//!
//! Posting `CGEvent`s requires Accessibility permission through `AXIsProcessTrusted`.
//! Callers check [`is_permitted`] and show a one-time grant hint when permission is absent.
//! `HOLOIROH_INPUT_DRY_RUN` permits event construction but suppresses posting.
//!
//! All injected events are synthetic and have a nonzero source process identifier (PID).
//! Therefore, the physical-input tap in `crate::user_activity` ignores them.
//! They represent remote user input, not local hardware activity.
//!
//! ## Secure fields
//!
//! [`text`] and [`key`] post through `CGEventTapLocation::HIDEventTap`.
//! This is the lowest-level system-wide injection point where hardware keystrokes arrive.
//! This file does not restrict posting to a particular session.
//!
//! Delivery to a focused secure field remains unverified.
//! `crate::permissions::secure_input_active` reports secure input focus.
//! Such focus includes the login window, lock screen, and `sudo` or Keychain prompts.
//! Apple documents `SecureEventInput` as protection against synthetic keystroke delivery to secure fields.
//! This protection applies to password-harvesting malware and legitimate remote typing.
//!
//! Testing did not use the user's lock screen.
//! A failed synthetic unlock could trigger password-attempt lockout on the user's account.
//! Verification requires a disposable test machine or the account owner's explicit, informed consent.

use std::sync::atomic::{AtomicBool, Ordering};

use objc2_core_foundation::{CGPoint, CGRect};
use objc2_core_graphics::{
    CGDisplayBounds, CGEvent, CGEventField, CGEventFlags, CGEventTapLocation, CGEventType,
    CGMainDisplayID, CGMouseButton, CGScrollEventUnit,
};

/// Tracks whether a mouse button is held.
/// A `Move` while held becomes a drag, which AppKit requires for click-and-drag input.
static LEFT_DOWN: AtomicBool = AtomicBool::new(false);
static RIGHT_DOWN: AtomicBool = AtomicBool::new(false);

/// Tracks the modifier keys that the remote client reports as held.
/// [`current_modifier_flags`] applies this state to plain letter and digit events from [`key`].
/// This combination produces shortcuts such as Cmd+C instead of the literal character `c`.
/// [`text`] bypasses held modifiers and shortcut interpretation.
/// Names identify physical keys to match [`keycode`] and iOS `UIKeyModifierFlags`.
static CMD_DOWN: AtomicBool = AtomicBool::new(false);
static CTRL_DOWN: AtomicBool = AtomicBool::new(false);
static OPT_DOWN: AtomicBool = AtomicBool::new(false);
static SHIFT_DOWN: AtomicBool = AtomicBool::new(false);

/// Returns the combined flags for held modifiers.
/// This module applies these flags to every keyboard and mouse event.
/// AppKit reads modifier state from mouse events for combinations such as Cmd+click and Shift+click.
fn current_modifier_flags() -> CGEventFlags {
    let mut flags = CGEventFlags::empty();
    if CMD_DOWN.load(Ordering::Relaxed) {
        flags |= CGEventFlags::MaskCommand;
    }
    if CTRL_DOWN.load(Ordering::Relaxed) {
        flags |= CGEventFlags::MaskControl;
    }
    if OPT_DOWN.load(Ordering::Relaxed) {
        flags |= CGEventFlags::MaskAlternate;
    }
    if SHIFT_DOWN.load(Ordering::Relaxed) {
        flags |= CGEventFlags::MaskShift;
    }
    flags
}

/// Reports whether the daemon can construct input events.
/// Accessibility permission allows posting.
/// `HOLOIROH_INPUT_DRY_RUN=1` permits event construction but suppresses posting.
pub fn is_permitted() -> bool {
    injection_is_dry_run() || crate::permissions::accessibility_granted()
}

/// Releases each held mouse button at the current cursor location.
/// This function also clears all held modifier state.
///
/// A client can disconnect during a drag before [`button`] receives the matching release.
/// This can happen when the app loses its connection or the user swipes it away.
/// It can also happen when the user lifts a finger over the letterbox.
/// The Mac then continues dragging or selecting until another input ends the operation.
/// The pointer drags anything it crosses until someone physically uses the Mac.
/// [`click`] also clears held mouse-button state.
///
/// Call this function safely and repeatedly.
/// It posts button-up events only for state that was set.
/// AppKit treats a synthetic button-up without a held button as a no-op.
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
    // Same class of bug as an abandoned mouse button: a client that disconnects mid-shortcut
    // (Cmd held, connection drops before the matching key-up) leaves the Mac believing Cmd is
    // still physically held, corrupting every subsequent local keystroke/click until someone
    // presses and releases Cmd by hand. Clear the flags unconditionally; a `key`/`click` with no
    // modifiers actually held is what every subsequent event already expects.
    CMD_DOWN.store(false, Ordering::Relaxed);
    CTRL_DOWN.store(false, Ordering::Relaxed);
    OPT_DOWN.store(false, Ordering::Relaxed);
    SHIFT_DOWN.store(false, Ordering::Relaxed);
}

/// Maps a normalized captured-display point to a global Core Graphics point.
/// Each input coordinate uses the inclusive range `0..=1` and is clamped to that range.
/// The daemon captures the primary display by default, so this function uses the primary display.
/// Support for a captured non-primary display is documented but not connected here.
pub fn map_normalized(nx: f64, ny: f64) -> CGPoint {
    let bounds = cached_display_bounds();
    let cx = nx.clamp(0.0, 1.0);
    let cy = ny.clamp(0.0, 1.0);
    CGPoint {
        x: bounds.origin.x + cx * bounds.size.width,
        y: bounds.origin.y + cy * bounds.size.height,
    }
}

/// Sets the display-bounds cache lifetime to 500 ms.
/// This duration avoids repeated lookups during a drag.
/// It also updates geometry after a resolution or display change.
const DISPLAY_BOUNDS_TTL: std::time::Duration = std::time::Duration::from_millis(500);

static DISPLAY_BOUNDS: std::sync::Mutex<Option<(std::time::Instant, CGRect)>> =
    std::sync::Mutex::new(None);

/// Returns cached primary-display bounds when the cache age is less than 500 ms.
/// At 500 ms or more, this function refreshes the bounds.
/// `CGDisplayBounds(CGMainDisplayID())` is a system call.
/// A drag can call this function 120 times each second on the inline read path.
/// Each call delays reading the next control message.
/// Measurement showed a 20 us p50 with 5 ms outliers.
/// A 5 ms outlier is one-third of a display frame of cursor-path jitter.
/// A key event without this lookup measured 167 ns.
/// The returned geometry changes only when the user changes displays.
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

/// Removes and returns all moves recorded under `HOLOIROH_INPUT_DRY_RUN`.
///
/// The daemon binary does not call this function.
/// The probes call it through the library target.
/// These probes are `remote_input_ordering_probe` and `input_latency_probe`.
/// Therefore, `allow(dead_code)` prevents an incorrect warning during each binary build.
#[allow(dead_code)]
pub fn take_applied_moves() -> Vec<(f64, f64)> {
    std::mem::take(&mut *APPLIED_MOVES.lock().unwrap_or_else(|e| e.into_inner()))
}

static APPLIED_CLICK_STATES: std::sync::Mutex<Vec<i64>> = std::sync::Mutex::new(Vec::new());

/// Removes and returns all click states recorded under `HOLOIROH_INPUT_DRY_RUN`.
/// The `click_state_probe` binary consumes these states through the library target.
/// See [`take_applied_moves`] for the library-versus-binary warning rationale.
#[allow(dead_code)]
pub fn take_applied_click_states() -> Vec<i64> {
    std::mem::take(
        &mut *APPLIED_CLICK_STATES
            .lock()
            .unwrap_or_else(|e| e.into_inner()),
    )
}

fn record_applied_click(states: &[i64], right: bool) {
    if !injection_is_dry_run() {
        return;
    }
    APPLIED_CLICK_STATES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .extend_from_slice(states);
    tracing::info!(
        input_kind = if right { "right_click" } else { "left_click" },
        click_count = states.len(),
        "remote input dry-run applied"
    );
}

/// Stores resolved key events as `(virtual keycode, down, applied modifier flags)`.
/// `HOLOIROH_INPUT_DRY_RUN` records these events without posting a `CGEvent`.
/// The `remote_input_key_probe` verifies the keycode table from these records.
/// It also verifies the held-modifier state machine.
/// An unknown key name records nothing because [`key`] posts nothing for unknown names.
static APPLIED_KEYS: std::sync::Mutex<Vec<(u16, bool, u64)>> = std::sync::Mutex::new(Vec::new());

/// Removes and returns all key events recorded under `HOLOIROH_INPUT_DRY_RUN`.
/// The `remote_input_key_probe` binary consumes these events through the library target.
/// See [`take_applied_moves`] for the library-versus-binary warning rationale.
#[allow(dead_code)]
pub fn take_applied_keys() -> Vec<(u16, bool, u64)> {
    std::mem::take(&mut *APPLIED_KEYS.lock().unwrap_or_else(|e| e.into_inner()))
}

fn record_applied_key(code: u16, down: bool, flags: CGEventFlags) {
    if !injection_is_dry_run() {
        return;
    }
    APPLIED_KEYS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((code, down, flags.0));
    tracing::info!(
        input_kind = "key",
        keycode = code,
        down,
        modifier_flags = flags.0,
        "remote input dry-run applied"
    );
}

fn record_applied_move(nx: f64, ny: f64) {
    if !injection_is_dry_run() {
        return;
    }
    APPLIED_MOVES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((nx, ny));
    tracing::info!(
        input_kind = "move",
        x = nx,
        y = ny,
        "remote input dry-run applied"
    );
}

fn post(event: &CGEvent) {
    if injection_is_dry_run() {
        return;
    }
    CGEvent::post(CGEventTapLocation::HIDEventTap, Some(event));
}

/// Returns the current cursor location in global Core Graphics points.
/// Witnesses and diagnostics use this value.
/// Returns `None` when Core Graphics cannot create an event.
pub fn cursor_location() -> Option<CGPoint> {
    let ev = CGEvent::new(None)?;
    Some(CGEvent::location(Some(&ev)))
}

/// Moves the cursor to a normalized point.
/// If a mouse button is held, the movement becomes a drag.
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
        CGEvent::set_flags(Some(&ev), current_modifier_flags());
        post(&ev);
    }
}

/// Presses or releases a mouse button at a normalized point.
/// Set `right` for the right button and `down` for a press.
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
    if injection_is_dry_run() {
        tracing::info!(
            input_kind = if right { "right_button" } else { "left_button" },
            x = nx,
            y = ny,
            down,
            "remote input dry-run applied"
        );
    }
    if let Some(ev) = CGEvent::new_mouse_event(None, ty, p, cgbtn) {
        CGEvent::set_flags(Some(&ev), current_modifier_flags());
        post(&ev);
    }
}

/// Sets the fixed double-click window to 500 ms.
/// This value matches the macOS default for `com.apple.mouse.doubleClickThreshold`.
/// The injection path does not read the user's preference.
/// Reading it would spawn a process during a click.
const DOUBLE_CLICK_WINDOW: std::time::Duration = std::time::Duration::from_millis(500);

/// Sets the movement slop to 6 display points on each axis.
/// The app maps a complete desktop onto a few hundred points.
/// Therefore, one touch-point of movement can span several desktop pixels.
const DOUBLE_CLICK_SLOP: f64 = 6.0;

static LAST_CLICK: std::sync::Mutex<Option<(std::time::Instant, CGPoint, bool, i64)>> =
    std::sync::Mutex::new(None);

/// Derives the click state from the previous click, as the window server does for hardware.
/// State `1` means single, `2` means double, and `3` means triple.
/// Consecutive clicks must use the same button.
/// They must occur no more than 500 ms apart.
/// Their movement must not exceed 6 display points on either axis.
/// Both limits include their boundary values.
///
/// The app cannot derive this state without delaying every tap for the double-click window.
/// That delay would add half a second to the most common interaction.
/// Local derivation does not add a wait.
/// It lets a fast double-tap open a folder like a hardware mouse.
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

/// Posts a complete mouse click at a normalized point.
/// A `count` greater than `1` forces that many click states.
/// A `count` of `1` lets [`next_click_state`] detect consecutive taps.
/// A `count` of `0` becomes `1`.
pub fn click(nx: f64, ny: f64, right: bool, count: u32) {
    let count = count.max(1);
    let p = map_normalized(nx, ny);
    let cgbtn = if right {
        CGMouseButton::Right
    } else {
        CGMouseButton::Left
    };
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
    record_applied_click(&states, right);
    let flags = current_modifier_flags();
    for state in states {
        if let Some(down) = CGEvent::new_mouse_event(None, dty, p, cgbtn) {
            CGEvent::set_integer_value_field(
                Some(&down),
                CGEventField::MouseEventClickState,
                state,
            );
            CGEvent::set_flags(Some(&down), flags);
            post(&down);
        }
        if let Some(up) = CGEvent::new_mouse_event(None, uty, p, cgbtn) {
            CGEvent::set_integer_value_field(Some(&up), CGEventField::MouseEventClickState, state);
            CGEvent::set_flags(Some(&up), flags);
            post(&up);
        }
    }
    // A click is not a drag; make sure no stale held-button state lingers.
    LEFT_DOWN.store(false, Ordering::Relaxed);
    RIGHT_DOWN.store(false, Ordering::Relaxed);
}

pub fn click_absolute(x: f64, y: f64, right: bool, count: u8) {
    if !x.is_finite() || !y.is_finite() {
        return;
    }
    let bounds = cached_display_bounds();
    if bounds.size.width <= 0.0 || bounds.size.height <= 0.0 {
        return;
    }
    let nx = (x - bounds.origin.x) / bounds.size.width;
    let ny = (y - bounds.origin.y) / bounds.size.height;
    if !(0.0..=1.0).contains(&nx) || !(0.0..=1.0).contains(&ny) {
        return;
    }
    click(nx, ny, right, u32::from(count));
}

pub fn scroll_absolute(x: f64, y: f64, dx: f64, dy: f64) {
    if ![x, y, dx, dy].iter().all(|value| value.is_finite()) {
        return;
    }
    let bounds = cached_display_bounds();
    if bounds.size.width <= 0.0 || bounds.size.height <= 0.0 {
        return;
    }
    let nx = (x - bounds.origin.x) / bounds.size.width;
    let ny = (y - bounds.origin.y) / bounds.size.height;
    if !(0.0..=1.0).contains(&nx) || !(0.0..=1.0).contains(&ny) {
        return;
    }
    scroll(nx, ny, dx, dy);
}

/// Scrolls at a normalized point with line-unit wheel deltas.
/// A negative `dy` scrolls content upward, matching a natural upward swipe.
pub fn scroll(nx: f64, ny: f64, dx: f64, dy: f64) {
    if injection_is_dry_run() {
        tracing::info!(
            input_kind = "scroll",
            x = nx,
            y = ny,
            dx,
            dy,
            "remote input dry-run applied"
        );
    }
    // Move the cursor to the point first so the scroll targets that spot.
    let p = map_normalized(nx, ny);
    if let Some(mv) =
        CGEvent::new_mouse_event(None, CGEventType::MouseMoved, p, CGMouseButton::Left)
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
        // A real trackpad's scroll carries whatever modifiers are held too (Shift+scroll for
        // horizontal in some apps, Option+scroll to zoom in others) -- same held-modifier state
        // key()/click() already apply.
        CGEvent::set_flags(Some(&ev), current_modifier_flags());
        post(&ev);
    }
}

/// Types a verbatim Unicode string at the current keyboard focus.
/// This function bypasses held modifiers and macOS shortcut interpretation.
/// An empty string posts nothing.
pub fn text(s: &str) {
    let utf16: Vec<u16> = s.encode_utf16().collect();
    if injection_is_dry_run() {
        tracing::info!(
            input_kind = "text",
            character_count = s.chars().count(),
            "remote input dry-run applied"
        );
    }
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

/// Presses or releases a named key.
/// Supported names include special keys, modifiers, letters, digits, and punctuation.
/// A modifier updates the held state for subsequent keyboard and mouse events.
/// [`current_modifier_flags`] provides that state.
/// Unknown special keys post nothing.
///
/// Use this function for shortcuts such as Cmd+C, Cmd+Tab, and Ctrl+A.
/// It posts keycode-based keyboard events with the applicable `CGEventFlags`.
/// In contrast, [`text`] injects literal Unicode through `keyboard_set_unicode_string`.
/// It bypasses held modifiers and shortcut interpretation.
/// Therefore, [`keycode`] includes printable keys and non-printable special keys.
/// Earlier versions limited this function to a small set of non-printable special keys.
pub fn key(name: &str, down: bool) {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "cmd" | "command" | "meta" => {
            CMD_DOWN.store(down, Ordering::Relaxed);
        }
        "ctrl" | "control" => {
            CTRL_DOWN.store(down, Ordering::Relaxed);
        }
        "opt" | "option" | "alt" => {
            OPT_DOWN.store(down, Ordering::Relaxed);
        }
        "shift" => {
            SHIFT_DOWN.store(down, Ordering::Relaxed);
        }
        _ => {}
    }
    let Some(code) = keycode(&lower) else {
        return;
    };
    let flags = current_modifier_flags();
    record_applied_key(code, down, flags);
    if let Some(ev) = CGEvent::new_keyboard_event(None, code, down) {
        CGEvent::set_flags(Some(&ev), flags);
        post(&ev);
    }
}

/// Maps a supported key name to its stable macOS HIToolbox `kVK_*` virtual keycode.
/// Returns `None` for unknown names, so [`key`] posts nothing.
/// Supports every key that iOS `UIKey.keyCode` can report during a hardware shortcut.
/// `UIKey.keyCode` uses `UIKeyboardHIDUsage`.
/// Also supports modifiers as key events because some apps read raw modifier keycodes instead of flags.
fn keycode(name: &str) -> Option<u16> {
    Some(match name {
        // Letters (kVK_ANSI_*), keyboard row order, not alphabetical -- copied directly from
        // the HIToolbox table, easiest to verify against Apple's own reference that way.
        "a" => 0,
        "s" => 1,
        "d" => 2,
        "f" => 3,
        "h" => 4,
        "g" => 5,
        "z" => 6,
        "x" => 7,
        "c" => 8,
        "v" => 9,
        "b" => 11,
        "q" => 12,
        "w" => 13,
        "e" => 14,
        "r" => 15,
        "y" => 16,
        "t" => 17,
        "o" => 31,
        "u" => 32,
        "i" => 34,
        "p" => 35,
        "l" => 37,
        "j" => 38,
        "k" => 40,
        "n" => 45,
        "m" => 46,
        // Digits.
        "1" => 18,
        "2" => 19,
        "3" => 20,
        "4" => 21,
        "6" => 22,
        "5" => 23,
        "9" => 25,
        "7" => 26,
        "8" => 28,
        "0" => 29,
        // Punctuation.
        "=" | "equal" => 24,
        "-" | "minus" => 27,
        "]" | "rightbracket" => 30,
        "[" | "leftbracket" => 33,
        "'" | "quote" => 39,
        ";" | "semicolon" => 41,
        "\\" | "backslash" => 42,
        "," | "comma" => 43,
        "/" | "slash" => 44,
        "." | "period" => 47,
        "`" | "grave" => 50,
        // Editing / whitespace.
        "return" | "enter" => 36,
        "delete" | "backspace" => 51,
        "forwarddelete" => 117,
        "escape" | "esc" => 53,
        "tab" => 48,
        "space" => 49,
        // Arrows.
        "left" => 123,
        "right" => 124,
        "down" => 125,
        "up" => 126,
        // Navigation.
        "home" => 115,
        "end" => 119,
        "pageup" => 116,
        "pagedown" => 121,
        // Function keys.
        "f1" => 122,
        "f2" => 120,
        "f3" => 99,
        "f4" => 118,
        "f5" => 96,
        "f6" => 97,
        "f7" => 98,
        "f8" => 100,
        "f9" => 101,
        "f10" => 109,
        "f11" => 103,
        "f12" => 111,
        // Modifiers (left-hand variants -- iOS reports left/right the same way to us either
        // side is pressed, and macOS treats either side identically for shortcut purposes).
        "cmd" | "command" | "meta" => 55,
        "shift" => 56,
        "capslock" => 57,
        "opt" | "option" | "alt" => 58,
        "ctrl" | "control" => 59,
        _ => return None,
    })
}
