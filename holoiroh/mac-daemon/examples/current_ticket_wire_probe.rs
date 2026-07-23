//! Pure-logic CI witness for `ServerMessage::CurrentTicket` (the daemon->app
//! current-ticket message that drives the app's identity-rotation ticket
//! refresh). Deterministic, no daemon needed -- it pins the exact JSON the iOS
//! `ServerMessage` decoder must parse, and confirms the new variant leaves
//! every existing `ServerMessage` kind round-tripping unchanged.
//!
//! Run with `cargo run --example current_ticket_wire_probe -p holoiroh-daemon`.

use holoiroh_daemon::control_channel::ServerMessage;

fn rt(msg: &ServerMessage) -> String {
    let json = serde_json::to_string(msg).expect("serialize");
    let back: ServerMessage = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(&back, msg, "round-trip mismatch for {json}");
    json
}

fn main() {
    let ticket = "iroh-live:nhWuOUavJaTyFA2AXzWPTiUUg38hFs6cOjKHKJu9pXwA/holoiroh";
    let msg = ServerMessage::current_ticket(ticket);
    let j = rt(&msg);
    println!("current_ticket -> {j}");
    assert!(j.contains("\"type\":\"current_ticket\""), "wrong type tag: {j}");
    assert!(j.contains(ticket), "ticket missing from wire: {j}");

    let decoded: ServerMessage =
        serde_json::from_str(r#"{"type":"current_ticket","ticket":"iroh-live:abc/holoiroh"}"#)
            .expect("decode canonical current_ticket");
    assert!(matches!(decoded, ServerMessage::CurrentTicket { .. }));

    for existing in [
        ServerMessage::ack(),
        ServerMessage::status("connected"),
        ServerMessage::error("boom"),
        ServerMessage::task_progress("clicking"),
        ServerMessage::task_done("completed", None),
        ServerMessage::auth_rejected("bad pin"),
        ServerMessage::TaskActive { paused: true, queued: 2 },
    ] {
        let j = rt(&existing);
        assert!(!j.contains("current_ticket"), "existing kind polluted by new variant: {j}");
    }

    println!(
        "current_ticket_wire_probe: OK -- CurrentTicket round-trips as the iOS decoder expects and existing kinds are unaffected."
    );
}
