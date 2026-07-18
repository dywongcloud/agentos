//! Manual, run-by-hand probe: exercises the real `TaskEnvelope`/`InboundEnvelopeState`
//! validation logic directly (expiry rejection, duplicate `message_id` rejection,
//! non-monotonic `sequence_number` rejection, and valid-in-order acceptance), against real
//! in-memory state, printing real accept/reject output for each case. Same pattern as
//! `examples/auth_gate_probe.rs` -- driven by `cargo run` rather than `cargo test`, per this
//! repo's no-unit-tests rule.
//!
//! This probe deliberately does NOT cover the pure `ClientMessage`/`ServerMessage`/
//! `TaskEnvelope` serde round-trips (see `examples/control_channel_probe.rs`) nor the
//! PIN/allowlist auth gate (see `examples/auth_gate_probe.rs`) -- it's scoped to exactly the
//! envelope-validation contract `InboundEnvelopeState::validate_inbound` implements:
//! expiry/dedup/sequence rejection, per the task this module was built for.
//!
//! Run with `cargo run --example envelope_probe`.

use holoiroh_daemon::control_channel::{
    EnvelopeRejection, InboundEnvelopeState, ServerMessage, TaskEnvelope,
};

/// Builds a `TaskEnvelope<ServerMessage>` (payload type is irrelevant to
/// `validate_inbound`, which only reads envelope-level fields, so `Ack` is
/// used throughout for brevity) with an explicit `expires_at` override
/// rather than the struct's own default-30s-from-now, so expiry cases can
/// be constructed deterministically instead of racing a real clock.
fn envelope_with_expiry(
    message_id: &str,
    sequence_number: u64,
    sent_at: u64,
    expires_at: u64,
) -> TaskEnvelope<ServerMessage> {
    TaskEnvelope {
        protocol_version: 1,
        message_id: message_id.to_string(),
        session_id: "session-probe".to_string(),
        task_id: None,
        message_type: "ack".to_string(),
        sent_at,
        expires_at,
        sequence_number,
        payload: ServerMessage::ack(),
        signature: None,
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn main() {
    println!("=== TaskEnvelope::new stamps a real default-30s expiry window ===");
    let fresh = TaskEnvelope::<ServerMessage>::wrap(
        "session-probe".to_string(),
        None,
        0,
        ServerMessage::ack(),
    );
    let window = fresh.expires_at - fresh.sent_at;
    println!("sent_at={} expires_at={} window_ms={}", fresh.sent_at, fresh.expires_at, window);
    assert_eq!(window, 30_000, "default expiry window must be exactly 30s");

    println!();
    println!("=== accepts a fresh, in-order, unique envelope ===");
    let mut state = InboundEnvelopeState::new();
    let now = now_ms();
    let env = envelope_with_expiry("msg-1", 0, now, now + 30_000);
    let result = state.validate_inbound(&env);
    println!("result -> {result:?}");
    assert_eq!(result, Ok(()));

    println!();
    println!("=== accepts strictly-increasing sequence_number on the same connection ===");
    let env2 = envelope_with_expiry("msg-2", 1, now, now + 30_000);
    let result = state.validate_inbound(&env2);
    println!("result -> {result:?}");
    assert_eq!(result, Ok(()), "sequence_number=1 must be accepted after sequence_number=0");

    println!();
    println!("=== rejects an expired envelope (now > expires_at) ===");
    let mut state = InboundEnvelopeState::new();
    let expired = envelope_with_expiry("msg-expired", 0, now - 60_000, now - 30_000);
    let result = state.validate_inbound(&expired);
    println!("result -> {result:?}");
    assert!(
        matches!(result, Err(EnvelopeRejection::Expired { .. })),
        "an envelope whose expires_at is in the past must be rejected as Expired"
    );

    println!();
    println!("=== accepts an envelope exactly AT its expires_at (only strictly-after is expired) ===");
    let mut state = InboundEnvelopeState::new();
    let now2 = now_ms();
    let boundary = envelope_with_expiry("msg-boundary", 0, now2 - 30_000, now2);
    let result = state.validate_inbound(&boundary);
    println!("result -> {result:?} (now={now2}, expires_at={})", boundary.expires_at);
    assert_eq!(result, Ok(()), "now == expires_at must NOT be treated as expired");

    println!();
    println!("=== rejects a duplicate message_id on the same connection ===");
    let mut state = InboundEnvelopeState::new();
    let first = envelope_with_expiry("msg-dup", 0, now, now + 30_000);
    let result1 = state.validate_inbound(&first);
    println!("first send  -> {result1:?}");
    assert_eq!(result1, Ok(()));
    let replay = envelope_with_expiry("msg-dup", 1, now, now + 30_000);
    let result2 = state.validate_inbound(&replay);
    println!("replay send -> {result2:?} (even with a higher sequence_number)");
    assert_eq!(
        result2,
        Err(EnvelopeRejection::DuplicateMessageId { message_id: "msg-dup".to_string() }),
        "a repeated message_id must be rejected even if sequence_number legitimately advanced"
    );

    println!();
    println!("=== rejects a non-increasing sequence_number (exact repeat) ===");
    let mut state = InboundEnvelopeState::new();
    let env_a = envelope_with_expiry("msg-seq-a", 5, now, now + 30_000);
    let result_a = state.validate_inbound(&env_a);
    println!("sequence_number=5 -> {result_a:?}");
    assert_eq!(result_a, Ok(()));
    let env_b = envelope_with_expiry("msg-seq-b", 5, now, now + 30_000);
    let result_b = state.validate_inbound(&env_b);
    println!("sequence_number=5 again (different message_id) -> {result_b:?}");
    assert_eq!(
        result_b,
        Err(EnvelopeRejection::SequenceNotMonotonic { got: 5, last_seen: 5 }),
        "a repeated sequence_number must be rejected even with a distinct message_id"
    );

    println!();
    println!("=== rejects a REGRESSING sequence_number ===");
    let mut state = InboundEnvelopeState::new();
    let env_high = envelope_with_expiry("msg-seq-high", 10, now, now + 30_000);
    let result_high = state.validate_inbound(&env_high);
    println!("sequence_number=10 -> {result_high:?}");
    assert_eq!(result_high, Ok(()));
    let env_low = envelope_with_expiry("msg-seq-low", 3, now, now + 30_000);
    let result_low = state.validate_inbound(&env_low);
    println!("sequence_number=3 (regression) -> {result_low:?}");
    assert_eq!(
        result_low,
        Err(EnvelopeRejection::SequenceNotMonotonic { got: 3, last_seen: 10 }),
        "a sequence_number lower than the last-seen one must be rejected"
    );

    println!();
    println!("=== a non-consecutive but still-increasing sequence_number is fine (gaps allowed) ===");
    let mut state = InboundEnvelopeState::new();
    let env_0 = envelope_with_expiry("msg-gap-0", 0, now, now + 30_000);
    assert_eq!(state.validate_inbound(&env_0), Ok(()));
    let env_100 = envelope_with_expiry("msg-gap-100", 100, now, now + 30_000);
    let result = state.validate_inbound(&env_100);
    println!("sequence_number jumps 0 -> 100 -> {result:?}");
    assert_eq!(result, Ok(()), "monotonic does not require consecutive -- gaps are fine");

    println!();
    println!("=== distinct connections (separate InboundEnvelopeState) do not share seen-sets or sequence state ===");
    let mut state_conn_a = InboundEnvelopeState::new();
    let mut state_conn_b = InboundEnvelopeState::new();
    let env = envelope_with_expiry("msg-shared-id", 0, now, now + 30_000);
    let result_a = state_conn_a.validate_inbound(&env);
    let result_b = state_conn_b.validate_inbound(&env);
    println!("connection A -> {result_a:?}");
    println!("connection B (same message_id/sequence_number) -> {result_b:?}");
    assert_eq!(result_a, Ok(()));
    assert_eq!(result_b, Ok(()), "a fresh connection's InboundEnvelopeState must not see connection A's seen-set");

    println!();
    println!("envelope_probe: OK -- all envelope validation cases witnessed via real execution");
}
