//! Manual, run-by-hand probe: exercises the real `ClientMessage`/`ServerMessage` JSON
//! serialize/deserialize round-trips and `ServerMessage::from_control_event` mapping from
//! `control_channel.rs`, printing real output. Witnesses the pure wire-schema logic that used
//! to live in `control_channel.rs`'s `#[cfg(test)] mod tests` (removed per this repo's
//! no-unit-tests rule).
//!
//! This probe deliberately does NOT cover `ControlChannel::authenticate`'s PIN/allowlist gate --
//! that logic is witnessed live against a real running daemon over a real `iroh` connection by
//! `examples/control_probe.rs` (see its own module doc), which is the more faithful live
//! witness for that stateful, async, network-facing behavior.
//!
//! Run with `cargo run --example control_channel_probe`.

use holoiroh_daemon::control_channel::{ClientMessage, ServerMessage};
use holoiroh_daemon::holo_bridge::ControlEvent;

fn round_trip_client(label: &str, msg: ClientMessage, expected_json: &str) {
    let json = serde_json::to_string(&msg).unwrap();
    println!("{label}: serialize -> {json}");
    assert_eq!(json, expected_json, "{label}: serialized JSON mismatch");
    let back: ClientMessage = serde_json::from_str(&json).unwrap();
    println!("{label}: deserialize -> {back:?}");
    assert_eq!(back, msg, "{label}: round-trip mismatch");
}

fn round_trip_server(label: &str, msg: ServerMessage, expected_json: &str) {
    let json = serde_json::to_string(&msg).unwrap();
    println!("{label}: serialize -> {json}");
    assert_eq!(json, expected_json, "{label}: serialized JSON mismatch");
    let back: ServerMessage = serde_json::from_str(&json).unwrap();
    println!("{label}: deserialize -> {back:?}");
    assert_eq!(back, msg, "{label}: round-trip mismatch");
}

fn main() {
    println!("=== ClientMessage round-trips ===");
    round_trip_client(
        "prompt",
        ClientMessage::Prompt {
            text: "open safari and check my calendar".to_string(),
        },
        r#"{"type":"prompt","text":"open safari and check my calendar"}"#,
    );
    round_trip_client(
        "voice_transcript",
        ClientMessage::VoiceTranscript {
            text: "what's on my screen right now".to_string(),
        },
        r#"{"type":"voice_transcript","text":"what's on my screen right now"}"#,
    );
    round_trip_client("stop", ClientMessage::Stop, r#"{"type":"stop"}"#);
    round_trip_client(
        "pin",
        ClientMessage::Pin {
            pin: "123456".to_string(),
        },
        r#"{"type":"pin","pin":"123456"}"#,
    );

    println!();
    println!("=== ServerMessage round-trips ===");
    round_trip_server("ack", ServerMessage::ack(), r#"{"type":"ack"}"#);
    round_trip_server(
        "status",
        ServerMessage::status("connected to holo-desktop-cli"),
        r#"{"type":"status","text":"connected to holo-desktop-cli"}"#,
    );
    round_trip_server(
        "task_progress",
        ServerMessage::task_progress("clicked Safari icon in the Dock"),
        r#"{"type":"task_progress","text":"clicked Safari icon in the Dock"}"#,
    );
    round_trip_server(
        "error",
        ServerMessage::error("holo-desktop-cli exited unexpectedly (code 1)"),
        r#"{"type":"error","text":"holo-desktop-cli exited unexpectedly (code 1)"}"#,
    );
    round_trip_server(
        "auth_rejected",
        ServerMessage::auth_rejected("incorrect PIN"),
        r#"{"type":"auth_rejected","text":"incorrect PIN"}"#,
    );

    println!();
    println!("=== malformed / unknown input: real deserialize errors, not panics ===");
    let malformed: Result<ClientMessage, _> = serde_json::from_str("not json");
    println!("serde_json::from_str(\"not json\") -> is_err={}", malformed.is_err());
    assert!(malformed.is_err());
    let unknown: Result<ClientMessage, _> = serde_json::from_str(r#"{"type":"unknown_variant"}"#);
    println!(
        "serde_json::from_str({{\"type\":\"unknown_variant\"}}) -> is_err={}",
        unknown.is_err()
    );
    assert!(unknown.is_err());

    println!();
    println!("=== ServerMessage::from_control_event mapping ===");
    let ack_event = ControlEvent::Ack {
        request_id: "r1".into(),
    };
    let mapped = ServerMessage::from_control_event(ack_event);
    println!("ControlEvent::Ack -> {mapped:?}");
    assert_eq!(mapped, ServerMessage::ack());

    let error_event = ControlEvent::Error {
        request_id: "r1".into(),
        message: "boom".into(),
    };
    let mapped = ServerMessage::from_control_event(error_event);
    println!("ControlEvent::Error -> {mapped:?}");
    assert_eq!(mapped, ServerMessage::error("boom"));

    let queued_event = ControlEvent::Queued {
        request_id: "r1".into(),
        ahead: 2,
    };
    let mapped = ServerMessage::from_control_event(queued_event);
    println!("ControlEvent::Queued{{ahead: 2}} -> {mapped:?}");
    assert_eq!(mapped, ServerMessage::status("queued, 2 ahead"));
    let json = serde_json::to_string(&mapped).unwrap();
    println!("  serialized -> {json}");
    assert_eq!(json, r#"{"type":"status","text":"queued, 2 ahead"}"#);

    let queued_zero_event = ControlEvent::Queued {
        request_id: "r1".into(),
        ahead: 0,
    };
    let mapped = ServerMessage::from_control_event(queued_zero_event);
    println!("ControlEvent::Queued{{ahead: 0}} -> {mapped:?}");
    assert_eq!(mapped, ServerMessage::status("queued, 0 ahead"));

    println!();
    println!("control_channel_probe: OK -- all wire-schema cases witnessed via real execution");
}
