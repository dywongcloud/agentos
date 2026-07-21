//! Manual, run-by-hand probe: exercises the real `ClientMessage`/`ServerMessage` JSON
//! serialize/deserialize round-trips (both bare, for the pre-session PIN handshake, and
//! envelope-wrapped via `TaskEnvelope<T>`, for every message after it -- see
//! `PROTOCOL.md`'s "Envelope" section for exactly where that line falls) and
//! `control_channel::from_control_event` mapping (a free function, not an inherent
//! `ServerMessage` method, since `ServerMessage` itself now lives in the `holoiroh-wire`
//! crate -- see `control_channel.rs`'s doc comment on `from_control_event` for why),
//! printing real output. Witnesses the pure wire-schema logic that used to live in
//! `control_channel.rs`'s `#[cfg(test)] mod tests` (removed per this repo's no-unit-tests
//! rule).
//!
//! This probe deliberately does NOT cover `ControlChannel::authenticate`'s PIN/allowlist gate,
//! nor the envelope-validation logic in `TaskEnvelope`/`InboundEnvelopeState`
//! (expiry/dedup/sequence rejection) -- the former is witnessed live against a real running
//! daemon over a real `iroh` connection by `examples/control_probe.rs` (see its own module
//! doc), and the latter by `examples/envelope_probe.rs`, both more faithful witnesses for that
//! stateful behavior than a pure serde round-trip probe would be.
//!
//! Run with `cargo run --example control_channel_probe`.

use holoiroh_daemon::control_channel;
use holoiroh_daemon::control_channel::{ClientMessage, ServerMessage, TaskEnvelope};
use holoiroh_daemon::holo_bridge::{ControlEvent, DoneStatus};

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
    println!("=== bare ClientMessage round-trips (pre-session PIN handshake wire shape) ===");
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
    println!("=== bare ServerMessage round-trips (pre-session auth_rejected wire shape) ===");
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
    println!("=== TaskEnvelope<ClientMessage> round-trip (post-session wire shape) ===");
    let client_envelope = TaskEnvelope::<ClientMessage>::wrap(
        "session-abc123".to_string(),
        Some("task-xyz789".to_string()),
        0,
        ClientMessage::Prompt {
            text: "open safari and check my calendar".to_string(),
        },
    );
    let json = serde_json::to_string(&client_envelope).unwrap();
    println!("serialize -> {json}");
    assert!(json.contains(r#""protocol_version":1"#));
    assert!(json.contains(r#""session_id":"session-abc123""#));
    assert!(json.contains(r#""task_id":"task-xyz789""#));
    assert!(json.contains(r#""message_type":"prompt""#));
    assert!(json.contains(r#""sequence_number":0"#));
    assert!(json.contains(r#""payload":{"type":"prompt","text":"open safari and check my calendar"}"#));
    assert!(!json.contains("\"signature\":"), "signature must be omitted when None, not emitted as null");
    let back: TaskEnvelope<ClientMessage> = serde_json::from_str(&json).unwrap();
    println!("deserialize -> {back:?}");
    assert_eq!(back, client_envelope, "envelope round-trip mismatch");
    assert_eq!(back.payload, ClientMessage::Prompt { text: "open safari and check my calendar".to_string() });

    println!();
    println!("=== TaskEnvelope<ServerMessage> round-trip (post-session wire shape) ===");
    let server_envelope = TaskEnvelope::<ServerMessage>::wrap(
        "session-abc123".to_string(),
        Some("task-xyz789".to_string()),
        1,
        ServerMessage::task_progress("clicked Safari icon in the Dock"),
    );
    let json = serde_json::to_string(&server_envelope).unwrap();
    println!("serialize -> {json}");
    assert!(json.contains(r#""message_type":"task_progress""#));
    assert!(json.contains(r#""sequence_number":1"#));
    let back: TaskEnvelope<ServerMessage> = serde_json::from_str(&json).unwrap();
    println!("deserialize -> {back:?}");
    assert_eq!(back, server_envelope, "envelope round-trip mismatch");

    println!();
    println!("=== TaskEnvelope omits task_id/signature when None (not emitted as null) ===");
    let no_task_id_envelope = TaskEnvelope::<ServerMessage>::wrap(
        "session-abc123".to_string(),
        None,
        0,
        ServerMessage::status("control channel ready"),
    );
    let json = serde_json::to_string(&no_task_id_envelope).unwrap();
    println!("serialize -> {json}");
    assert!(!json.contains("\"task_id\":"), "task_id must be omitted when None, not emitted as null");
    assert!(!json.contains("\"signature\":"), "signature must be omitted when None, not emitted as null");

    println!();
    println!("=== TaskEnvelope::is_expired_at ===");
    let envelope = TaskEnvelope::<ServerMessage>::wrap(
        "session-abc123".to_string(),
        None,
        0,
        ServerMessage::ack(),
    );
    println!(
        "sent_at={} expires_at={} (default 30s window)",
        envelope.sent_at, envelope.expires_at
    );
    assert_eq!(envelope.expires_at - envelope.sent_at, 30_000, "default expiry window must be 30s");
    assert!(!envelope.is_expired_at(envelope.sent_at), "must not be expired at sent_at");
    assert!(!envelope.is_expired_at(envelope.expires_at), "must not be expired exactly at expires_at (only strictly after)");
    assert!(envelope.is_expired_at(envelope.expires_at + 1), "must be expired 1ms past expires_at");
    println!("is_expired_at checks: OK");

    println!();
    println!("=== control_channel::from_control_event mapping ===");
    let ack_event = ControlEvent::Ack {
        request_id: "r1".into(),
    };
    let mapped = control_channel::from_control_event(ack_event);
    println!("ControlEvent::Ack -> {mapped:?}");
    assert_eq!(mapped, ServerMessage::ack());

    let error_event = ControlEvent::Error {
        request_id: "r1".into(),
        message: "boom".into(),
    };
    let mapped = control_channel::from_control_event(error_event);
    println!("ControlEvent::Error -> {mapped:?}");
    assert_eq!(mapped, ServerMessage::error("boom"));

    let queued_event = ControlEvent::Queued {
        request_id: "r1".into(),
        ahead: 2,
    };
    let mapped = control_channel::from_control_event(queued_event);
    println!("ControlEvent::Queued{{ahead: 2}} -> {mapped:?}");
    assert_eq!(mapped, ServerMessage::status("queued, 2 ahead"));
    let json = serde_json::to_string(&mapped).unwrap();
    println!("  serialized -> {json}");
    assert_eq!(json, r#"{"type":"status","text":"queued, 2 ahead"}"#);

    let queued_zero_event = ControlEvent::Queued {
        request_id: "r1".into(),
        ahead: 0,
    };
    let mapped = control_channel::from_control_event(queued_zero_event);
    println!("ControlEvent::Queued{{ahead: 0}} -> {mapped:?}");
    assert_eq!(mapped, ServerMessage::status("queued, 0 ahead"));

    // Terminal lifecycle reaches the wire as first-class `task_done` frames (the phone's
    // task controls key off them) -- `status` carries the snake_case DoneStatus name and
    // the client styles `failed` as an error row itself. This replaced the older
    // Failed->error / Completed->status folding.
    let done_failed_event = ControlEvent::Done {
        request_id: "r1".into(),
        context_id: None,
        status: DoneStatus::Failed,
        message: Some("agent backend error".into()),
    };
    let mapped = control_channel::from_control_event(done_failed_event);
    println!("ControlEvent::Done{{status: Failed}} -> {mapped:?}");
    assert_eq!(
        mapped,
        ServerMessage::task_done("failed", Some("agent backend error".into()))
    );
    let json = serde_json::to_string(&mapped).unwrap();
    println!("  serialized -> {json}");
    assert_eq!(
        json,
        r#"{"type":"task_done","status":"failed","text":"agent backend error"}"#
    );

    let done_completed_event = ControlEvent::Done {
        request_id: "r1".into(),
        context_id: None,
        status: DoneStatus::Completed,
        message: None,
    };
    let mapped = control_channel::from_control_event(done_completed_event);
    println!("ControlEvent::Done{{status: Completed}} -> {mapped:?}");
    assert_eq!(mapped, ServerMessage::task_done("completed", None));

    let done_canceled_event = ControlEvent::Done {
        request_id: "r1".into(),
        context_id: None,
        status: DoneStatus::Canceled,
        message: Some("stop requested".into()),
    };
    let mapped = control_channel::from_control_event(done_canceled_event);
    println!("ControlEvent::Done{{status: Canceled}} -> {mapped:?}");
    assert_eq!(
        mapped,
        ServerMessage::task_done("canceled", Some("stop requested".into()))
    );

    println!();
    println!("control_channel_probe: OK -- all wire-schema cases witnessed via real execution");
}
