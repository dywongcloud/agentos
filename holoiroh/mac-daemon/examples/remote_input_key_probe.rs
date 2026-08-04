//! Witnesses `remote_input`'s keycode table and held-modifier state machine (the pieces added
//! for real hardware-keyboard shortcut support: Cmd+C, Ctrl+A, etc.) without posting a real
//! CGEvent.
//!
//! Runs under `HOLOIROH_INPUT_DRY_RUN=1`, which records `(keycode, down, flags)` via
//! `take_applied_keys` instead of injecting real keystrokes, so this cannot type/press anything
//! on the machine running it.

use holoiroh_daemon::remote_input;

const MASK_SHIFT: u64 = 131072;
const MASK_CONTROL: u64 = 262144;
const MASK_ALTERNATE: u64 = 524288;
const MASK_COMMAND: u64 = 1048576;

fn main() {
    assert_eq!(
        std::env::var("HOLOIROH_INPUT_DRY_RUN").as_deref(),
        Ok("1"),
        "this probe must run with HOLOIROH_INPUT_DRY_RUN=1 so it records key events instead of \
         typing/pressing keys on the machine running it"
    );

    // --- unknown key name: ignored, records nothing ---
    let _ = remote_input::take_applied_keys();
    remote_input::key("not-a-real-key-name", true);
    assert!(
        remote_input::take_applied_keys().is_empty(),
        "an unrecognized key name must be silently ignored, not posted as some fallback"
    );
    println!("unknown key name: OK -- silently ignored");

    // --- plain special key, no modifiers held: flags are 0 ---
    remote_input::key("escape", true);
    remote_input::key("escape", false);
    let events = remote_input::take_applied_keys();
    assert_eq!(
        events.len(),
        2,
        "escape down+up must record exactly 2 events, got {events:?}"
    );
    assert_eq!(
        events[0],
        (53, true, 0),
        "escape keycode must be 53 (kVK_Escape), flags 0: {events:?}"
    );
    assert_eq!(
        events[1],
        (53, false, 0),
        "escape up must match: {events:?}"
    );
    println!("plain escape: OK -- keycode 53, no modifier flags");

    // --- Cmd+C shortcut: cmd down (updates state, ALSO posts a real keycode 55 event), then
    // 'c' down+up carries the Command flag, then cmd up clears it ---
    remote_input::key("cmd", true);
    remote_input::key("c", true);
    remote_input::key("c", false);
    remote_input::key("cmd", false);
    let events = remote_input::take_applied_keys();
    assert_eq!(
        events.len(),
        4,
        "cmd+c+c+cmd must record exactly 4 events, got {events:?}"
    );
    // Matches real macOS hardware: a modifier's own flagsChanged event reports the flags state
    // AT delivery time, which already includes the key just pressed -- so cmd-down itself
    // carries MaskCommand, and cmd-up (state already cleared before this event is built)
    // carries none.
    assert_eq!(
        events[0],
        (55, true, MASK_COMMAND),
        "cmd-down must report itself as active: {events:?}"
    );
    assert_eq!(
        events[1].0, 8,
        "'c' must resolve to keycode 8 (kVK_ANSI_C): {events:?}"
    );
    assert_eq!(events[1].1, true);
    assert_eq!(
        events[1].2 & MASK_COMMAND,
        MASK_COMMAND,
        "'c' pressed while cmd held must carry MaskCommand: {events:?}"
    );
    assert_eq!(
        events[2],
        (8, false, MASK_COMMAND),
        "'c' release must still carry the Command flag: {events:?}"
    );
    assert_eq!(events[3], (55, false, 0), "cmd-up itself: {events:?}");
    println!("Cmd+C: OK -- 'c' down+up both carry MaskCommand while cmd is held");

    // --- multiple modifiers combine: Ctrl+Shift+Tab ---
    remote_input::key("ctrl", true);
    remote_input::key("shift", true);
    remote_input::key("tab", true);
    let events = remote_input::take_applied_keys();
    let tab_event = events.last().expect("tab event must be recorded");
    assert_eq!(tab_event.0, 48, "tab keycode must be 48: {events:?}");
    assert_eq!(
        tab_event.2 & (MASK_CONTROL | MASK_SHIFT),
        MASK_CONTROL | MASK_SHIFT,
        "tab pressed under ctrl+shift must carry both flags: {events:?}"
    );
    println!("Ctrl+Shift+Tab: OK -- both modifier flags combined");
    // Clean up so release_all's test below starts from a known state.
    remote_input::key("tab", false);
    remote_input::key("shift", false);
    remote_input::key("ctrl", false);
    let _ = remote_input::take_applied_keys();

    // --- release_all clears held-modifier state even mid-shortcut (simulates a dropped
    // connection with cmd held and no matching key-up ever arriving) ---
    remote_input::key("cmd", true);
    let _ = remote_input::take_applied_keys();
    remote_input::release_all();
    remote_input::key("v", true);
    let events = remote_input::take_applied_keys();
    let v_event = events
        .iter()
        .find(|e| e.0 == 9)
        .expect("'v' event must be recorded");
    assert_eq!(
        v_event.2 & MASK_COMMAND,
        0,
        "release_all must have cleared cmd -- 'v' must NOT carry MaskCommand: {events:?}"
    );
    println!(
        "release_all: OK -- abandoned modifier state is cleared, does not leak into the next key"
    );

    // --- Option+letter and every documented modifier alias resolve the same way ---
    for alias in ["opt", "option", "alt"] {
        remote_input::key(alias, true);
        let events = remote_input::take_applied_keys();
        assert_eq!(
            events,
            vec![(58, true, MASK_ALTERNATE)],
            "'{alias}' must resolve to the Option keycode (58): {events:?}"
        );
        remote_input::key(alias, false);
        let _ = remote_input::take_applied_keys();
    }
    println!("modifier aliases (opt/option/alt): OK -- all resolve to keycode 58");

    println!(
        "remote_input_key_probe: OK -- keycode table and held-modifier flag composition behave correctly for real shortcut support."
    );
}
