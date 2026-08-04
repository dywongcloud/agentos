//! Pure-logic CI witness for `ServerMessage::SecureInputState` (the
//! daemon->app lock-screen signal from `secure_input_watchdog`) and its
//! translation from `holo_bridge::control::ControlEvent::SecureInputState`.
//! Deterministic, no daemon/device needed -- it pins the exact JSON the iOS
//! `ServerMessage` decoder must parse, confirms `from_control_event`
//! translates the bridge event correctly, and confirms every existing
//! `ServerMessage` kind round-trips unaffected by the new variant.
//!
//! Run with `cargo run --example secure_input_state_wire_probe -p holoiroh-daemon`.

use holoiroh_daemon::control_channel::{ServerMessage, from_control_event};
use holoiroh_daemon::holo_bridge::control::ControlEvent;

fn rt(msg: &ServerMessage) -> String {
    let json = serde_json::to_string(msg).expect("serialize");
    let back: ServerMessage = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(&back, msg, "round-trip mismatch for {json}");
    json
}

fn main() {
    for active in [true, false] {
        let msg = ServerMessage::SecureInputState { active };
        let j = rt(&msg);
        println!("secure_input_state(active={active}) -> {j}");
        assert!(
            j.contains("\"type\":\"secure_input_state\""),
            "wrong type tag: {j}"
        );
        assert!(
            j.contains(&format!("\"active\":{active}")),
            "active flag missing: {j}"
        );
    }

    let decoded: ServerMessage =
        serde_json::from_str(r#"{"type":"secure_input_state","active":true}"#)
            .expect("decode canonical secure_input_state");
    assert!(matches!(
        decoded,
        ServerMessage::SecureInputState { active: true }
    ));

    let translated = from_control_event(ControlEvent::SecureInputState { active: true });
    assert!(
        matches!(translated, ServerMessage::SecureInputState { active: true }),
        "ControlEvent::SecureInputState did not translate to ServerMessage::SecureInputState: {translated:?}"
    );

    for existing in [
        ServerMessage::ack(),
        ServerMessage::status("connected"),
        ServerMessage::error("boom"),
        ServerMessage::task_progress("clicking"),
        ServerMessage::task_done("completed", None),
        ServerMessage::auth_rejected("bad pin"),
        ServerMessage::current_ticket("iroh-live:abc/holoiroh"),
        ServerMessage::TaskActive {
            paused: true,
            queued: 2,
        },
    ] {
        let j = rt(&existing);
        assert!(
            !j.contains("secure_input_state"),
            "existing kind polluted by new variant: {j}"
        );
    }

    println!(
        "secure_input_state_wire_probe: OK -- SecureInputState round-trips as the iOS decoder expects, translates correctly from ControlEvent, and existing kinds are unaffected."
    );
}
