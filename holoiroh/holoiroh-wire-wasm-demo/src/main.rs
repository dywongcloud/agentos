//! WASI witness for session validation and canonical signing bytes.
//!
//! Run with `wasmtime target/wasm32-wasip1/debug/holoiroh-wire-wasm-demo.wasm`.

use holoiroh_wire::{
    ClientMessage, EnvelopeDirection, EnvelopeRejection, InboundEnvelopeState, TaskEnvelope,
    decode_ed25519_signature, encode_ed25519_signature,
};

fn envelope(session_id: &str) -> TaskEnvelope<ClientMessage> {
    TaskEnvelope {
        protocol_version: 1,
        message_id: "wasm-message-1".to_string(),
        session_id: session_id.to_string(),
        task_id: Some("wasm-task-1".to_string()),
        message_type: "prompt".to_string(),
        sent_at: 1,
        expires_at: u64::MAX,
        sequence_number: 0,
        payload: ClientMessage::Prompt {
            text: "open Safari".to_string(),
        },
        signature: None,
    }
}

fn main() {
    let signer = [0x31; 32];
    let recipient = [0x42; 32];
    let valid = envelope("wasm-session");

    let first = valid
        .signing_payload(EnvelopeDirection::ClientToDaemon, &signer, &recipient)
        .expect("canonicalize envelope");
    let repeated = valid
        .signing_payload(EnvelopeDirection::ClientToDaemon, &signer, &recipient)
        .expect("canonicalize envelope again");
    assert_eq!(first, repeated);

    let opposite = valid
        .signing_payload(EnvelopeDirection::DaemonToClient, &signer, &recipient)
        .expect("canonicalize opposite direction");
    assert_ne!(first, opposite);
    println!("WASI canonicalization OK: stable bytes and direction binding");

    let encoded = encode_ed25519_signature(&[0xab; 64]);
    assert_eq!(decode_ed25519_signature(&encoded), Ok([0xab; 64]));
    println!("WASI signature codec OK: strict format round-trips");

    let mut state = InboundEnvelopeState::for_session("wasm-session");
    let wrong = envelope("wrong-session");
    assert!(matches!(
        state.validate_inbound(&wrong),
        Err(EnvelopeRejection::SessionMismatch { .. })
    ));
    assert_eq!(state.validate_inbound(&valid), Ok(()));
    assert!(matches!(
        state.validate_inbound(&valid),
        Err(EnvelopeRejection::DuplicateMessageId { .. })
    ));
    println!("WASI session validation OK: mismatch is non-consuming and replay is rejected");

    let json = serde_json::to_string(&valid).expect("serialize envelope");
    let decoded: TaskEnvelope<ClientMessage> =
        serde_json::from_str(&json).expect("deserialize envelope");
    assert_eq!(decoded, valid);
    assert_eq!(decoded.signature, None);
    println!("WASI serde OK: envelope round-trip preserves signature=None");

    println!("holoiroh-wire-wasm-demo: ALL CHECKS PASSED under wasm32-wasip1");
}
