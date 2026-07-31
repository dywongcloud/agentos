//! Real wasm32-wasip1 witness for `holoiroh-wire`: builds a `TaskEnvelope<ClientMessage>`,
//! round-trips it through JSON, and exercises `InboundEnvelopeState::validate_inbound`'s
//! expiry/replay/sequence checks, entirely inside a WASI sandbox with no daemon, no `iroh`,
//! no macOS API. Run with `wasmtime target/wasm32-wasip1/debug/holoiroh-wire-wasm-demo.wasm`.

use holoiroh_wire::{ClientMessage, EnvelopeRejection, InboundEnvelopeState, TaskEnvelope};

fn main() {
    let envelope = TaskEnvelope::<ClientMessage>::wrap(
        "wasm-demo-session".to_string(),
        Some("wasm-demo-task".to_string()),
        0,
        ClientMessage::Prompt {
            text: "open Safari".to_string(),
        },
    );

    let json = serde_json::to_string_pretty(&envelope).expect("serialize envelope");
    println!("serialized envelope:\n{json}");

    let decoded: TaskEnvelope<ClientMessage> =
        serde_json::from_str(&json).expect("deserialize envelope");
    assert_eq!(decoded, envelope, "round-trip must be byte-for-byte equal");
    println!("round-trip OK: decoded envelope equals the original");

    let mut state = InboundEnvelopeState::new();

    state
        .validate_inbound(&decoded)
        .expect("first envelope must validate");
    println!("validate_inbound OK: sequence_number=0 accepted");

    match state.validate_inbound(&decoded) {
        Err(EnvelopeRejection::DuplicateMessageId { message_id }) => {
            println!("replay rejection OK: duplicate message_id={message_id}");
        }
        other => panic!("expected DuplicateMessageId rejection, got {other:?}"),
    }

    let stale_sequence = TaskEnvelope::<ClientMessage>::wrap(
        "wasm-demo-session".to_string(),
        Some("wasm-demo-task".to_string()),
        0,
        ClientMessage::Prompt {
            text: "second message, same sequence number".to_string(),
        },
    );
    match state.validate_inbound(&stale_sequence) {
        Err(EnvelopeRejection::SequenceNotMonotonic { got, last_seen }) => {
            println!("sequence rejection OK: got={got} last_seen={last_seen}");
        }
        other => panic!("expected SequenceNotMonotonic rejection, got {other:?}"),
    }

    let mut expired = TaskEnvelope::<ClientMessage>::wrap(
        "wasm-demo-session".to_string(),
        Some("wasm-demo-task".to_string()),
        1,
        ClientMessage::Prompt {
            text: "already expired".to_string(),
        },
    );
    expired.expires_at = 0;
    match state.validate_inbound(&expired) {
        Err(EnvelopeRejection::Expired { expires_at, now }) => {
            println!("expiry rejection OK: expires_at={expires_at} now={now}");
        }
        other => panic!("expected Expired rejection, got {other:?}"),
    }

    println!("holoiroh-wire-wasm-demo: ALL CHECKS PASSED under wasm32-wasip1");
}
