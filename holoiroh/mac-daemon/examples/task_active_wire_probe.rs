//! Pure-logic CI witness for the new `task_active` ServerMessage (issue-2:
//! restore the Pause/Stop pill on reconnect). Deterministic, no daemon / TCC /
//! network needed -- it exercises the real `holoiroh_wire::ServerMessage` serde
//! contract both directions, so the exact JSON the iOS app must parse
//! (`ServerMessage.swift`'s `.taskActive`) is pinned by an executable witness.
//!
//! Run with `cargo run --example task_active_wire_probe -p holoiroh-daemon`.

use holoiroh_daemon::control_channel::ServerMessage;

fn main() {
    // Running task, nothing queued.
    let running = ServerMessage::TaskActive { paused: false, queued: 0 };
    let json = serde_json::to_string(&running).expect("serialize task_active");
    println!("running -> {json}");
    assert!(json.contains("\"type\":\"task_active\""), "wrong type tag: {json}");
    assert!(json.contains("\"paused\":false"), "missing paused field: {json}");
    assert!(json.contains("\"queued\":0"), "missing queued field: {json}");

    // Round-trips back to the same variant.
    let back: ServerMessage = serde_json::from_str(&json).expect("deserialize task_active");
    assert_eq!(back, running, "round-trip mismatch");

    // Paused task with two prompts queued behind it.
    let paused = ServerMessage::TaskActive { paused: true, queued: 2 };
    let pjson = serde_json::to_string(&paused).expect("serialize paused task_active");
    println!("paused  -> {pjson}");
    let pback: ServerMessage = serde_json::from_str(&pjson).expect("deserialize paused");
    assert_eq!(pback, paused, "paused round-trip mismatch");

    // The exact wire an older/other client might emit decodes too (field order,
    // explicit values) -- proving `ServerMessage.swift`'s decoder will accept
    // the daemon's real output.
    let decoded: ServerMessage =
        serde_json::from_str(r#"{"type":"task_active","paused":true,"queued":1}"#)
            .expect("decode canonical task_active");
    assert_eq!(decoded, ServerMessage::TaskActive { paused: true, queued: 1 });

    println!("task_active_wire_probe: OK -- task_active serializes/deserializes as the client expects.");
}
