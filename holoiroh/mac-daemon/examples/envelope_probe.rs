//! Executable probe for canonical envelope signing and inbound validation.
//!
//! Run with `cargo run --example envelope_probe`.

use holoiroh_daemon::control_channel::{
    ClientMessage, ServerMessage, sign_daemon_envelope_for_probing,
    verify_client_envelope_for_probing,
};
use holoiroh_wire::{
    EnvelopeDirection, EnvelopeRejection, InboundEnvelopeState, SignatureCodecError, TaskEnvelope,
    decode_ed25519_signature, encode_ed25519_signature,
};
use iroh::{SecretKey, Signature};
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use serde_json::{Value, json};

#[derive(Clone)]
struct InsertionOrderedObject(Vec<(&'static str, Value)>);

impl Serialize for InsertionOrderedObject {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

fn signing_envelope(payload: InsertionOrderedObject) -> TaskEnvelope<InsertionOrderedObject> {
    TaskEnvelope {
        protocol_version: 1,
        message_id: "message-0001".to_string(),
        session_id: "session-probe".to_string(),
        task_id: Some("task-0001".to_string()),
        message_type: "probe".to_string(),
        sent_at: 1_725_000_000_000,
        expires_at: u64::MAX,
        sequence_number: 7,
        payload,
        signature: None,
    }
}

fn ordered_payload(reverse: bool) -> InsertionOrderedObject {
    let nested = json!({
        "unicode-é": [null, true, false, -12.5],
        "alpha": "value"
    });
    if reverse {
        InsertionOrderedObject(vec![
            ("zeta", json!(9)),
            ("nested", nested),
            ("alpha", json!("first")),
        ])
    } else {
        InsertionOrderedObject(vec![
            ("alpha", json!("first")),
            ("nested", nested),
            ("zeta", json!(9)),
        ])
    }
}

fn canonical_bytes(
    envelope: &TaskEnvelope<InsertionOrderedObject>,
    direction: EnvelopeDirection,
    signer: &[u8; 32],
    recipient: &[u8; 32],
) -> Vec<u8> {
    envelope
        .signing_payload(direction, signer, recipient)
        .expect("probe payload must canonicalize")
}

fn assert_envelope_mutation_changes(
    name: &str,
    baseline: &[u8],
    envelope: &TaskEnvelope<InsertionOrderedObject>,
    signer: &[u8; 32],
    recipient: &[u8; 32],
) {
    let changed = canonical_bytes(
        envelope,
        EnvelopeDirection::ClientToDaemon,
        signer,
        recipient,
    );
    assert_ne!(
        baseline, changed,
        "{name} mutation must change signing bytes"
    );
    println!("canonical mutation OK: {name}");
}

fn simple_envelope(
    message_id: &str,
    session_id: &str,
    sequence_number: u64,
) -> TaskEnvelope<Value> {
    TaskEnvelope {
        protocol_version: 1,
        message_id: message_id.to_string(),
        session_id: session_id.to_string(),
        task_id: None,
        message_type: "probe".to_string(),
        sent_at: 1,
        expires_at: u64::MAX,
        sequence_number,
        payload: json!({"ok": true}),
        signature: None,
    }
}

fn probe_canonical_signing() {
    let signer = [0x11; 32];
    let recipient = [0x22; 32];
    let envelope = signing_envelope(ordered_payload(false));
    let baseline = canonical_bytes(
        &envelope,
        EnvelopeDirection::ClientToDaemon,
        &signer,
        &recipient,
    );
    let repeated = canonical_bytes(
        &envelope,
        EnvelopeDirection::ClientToDaemon,
        &signer,
        &recipient,
    );
    assert_eq!(baseline, repeated);
    println!("canonical equality OK: repeated encoding is byte-identical");

    for domain_part in [
        b"holoiroh/control/1".as_slice(),
        b"task-envelope".as_slice(),
        b"ed25519".as_slice(),
        b"signature-v1".as_slice(),
    ] {
        assert!(
            baseline
                .windows(domain_part.len())
                .any(|window| window == domain_part)
        );
    }
    let mut ndjson = serde_json::to_vec(&envelope).expect("serialize probe envelope");
    ndjson.push(b'\n');
    assert_ne!(baseline, ndjson);
    println!("domain separation OK: all four labels present and bytes are not NDJSON");

    let reversed = signing_envelope(ordered_payload(true));
    let reversed_bytes = canonical_bytes(
        &reversed,
        EnvelopeDirection::ClientToDaemon,
        &signer,
        &recipient,
    );
    assert_eq!(baseline, reversed_bytes);
    println!("canonical object order OK: insertion order does not change bytes");

    let mut changed = envelope.clone();
    changed.protocol_version += 1;
    assert_envelope_mutation_changes("protocol_version", &baseline, &changed, &signer, &recipient);

    let mut changed = envelope.clone();
    changed.message_id.push('x');
    assert_envelope_mutation_changes("message_id", &baseline, &changed, &signer, &recipient);

    let mut changed = envelope.clone();
    changed.session_id.push('x');
    assert_envelope_mutation_changes("session_id", &baseline, &changed, &signer, &recipient);

    let mut changed = envelope.clone();
    changed.task_id = None;
    assert_envelope_mutation_changes("task_id", &baseline, &changed, &signer, &recipient);

    let mut changed = envelope.clone();
    changed.message_type.push('x');
    assert_envelope_mutation_changes("message_type", &baseline, &changed, &signer, &recipient);

    let mut changed = envelope.clone();
    changed.sent_at += 1;
    assert_envelope_mutation_changes("sent_at", &baseline, &changed, &signer, &recipient);

    let mut changed = envelope.clone();
    changed.expires_at -= 1;
    assert_envelope_mutation_changes("expires_at", &baseline, &changed, &signer, &recipient);

    let mut changed = envelope.clone();
    changed.sequence_number += 1;
    assert_envelope_mutation_changes("sequence_number", &baseline, &changed, &signer, &recipient);

    let mut changed = envelope.clone();
    changed.payload.0[0].1 = json!("changed");
    assert_envelope_mutation_changes("payload", &baseline, &changed, &signer, &recipient);

    let opposite_direction = canonical_bytes(
        &envelope,
        EnvelopeDirection::DaemonToClient,
        &signer,
        &recipient,
    );
    assert_ne!(baseline, opposite_direction);
    println!("canonical mutation OK: direction");

    let mut changed_signer = signer;
    changed_signer[31] ^= 1;
    assert_ne!(
        baseline,
        canonical_bytes(
            &envelope,
            EnvelopeDirection::ClientToDaemon,
            &changed_signer,
            &recipient,
        )
    );
    println!("canonical mutation OK: signer");

    let mut changed_recipient = recipient;
    changed_recipient[31] ^= 1;
    assert_ne!(
        baseline,
        canonical_bytes(
            &envelope,
            EnvelopeDirection::ClientToDaemon,
            &signer,
            &changed_recipient,
        )
    );
    println!("canonical mutation OK: recipient");

    let mut signed = envelope;
    signed.signature = Some(encode_ed25519_signature(&[0xab; 64]));
    assert_eq!(
        baseline,
        canonical_bytes(
            &signed,
            EnvelopeDirection::ClientToDaemon,
            &signer,
            &recipient,
        )
    );
    println!("signature exclusion OK: signature does not change signing bytes");
}

fn probe_signature_codec() {
    let signature = [0xab; 64];
    let encoded = encode_ed25519_signature(&signature);
    assert_eq!(encoded, format!("ed25519:{}", "ab".repeat(64)));
    assert_eq!(decode_ed25519_signature(&encoded), Ok(signature));

    let uppercase = format!("ed25519:{}", "AB".repeat(64));
    assert!(matches!(
        decode_ed25519_signature(&uppercase),
        Err(SignatureCodecError::UppercaseHex { .. })
    ));
    assert_eq!(
        decode_ed25519_signature(&format!("ed448:{}", "ab".repeat(64))),
        Err(SignatureCodecError::WrongPrefix)
    );
    assert_eq!(
        decode_ed25519_signature(&format!("ed25519:{}", "ab".repeat(63))),
        Err(SignatureCodecError::WrongLength { got: 126 })
    );
    assert_eq!(
        decode_ed25519_signature(&format!("ed25519:{}", "ab".repeat(65))),
        Err(SignatureCodecError::WrongLength { got: 130 })
    );
    assert!(matches!(
        decode_ed25519_signature(&format!("ed25519:{}g", "ab".repeat(63) + "a")),
        Err(SignatureCodecError::NonHex { .. })
    ));
    println!("signature codec OK: round-trip and adversarial rejection cases passed");
}

fn probe_session_and_replay_state() {
    let mut state = InboundEnvelopeState::for_session("session-probe");
    let wrong_session = simple_envelope("message-shared", "wrong-session", 0);
    assert_eq!(
        state.validate_inbound(&wrong_session),
        Err(EnvelopeRejection::SessionMismatch {
            expected: "session-probe".to_string(),
            got: "wrong-session".to_string(),
        })
    );

    let valid_after_mismatch = simple_envelope("message-shared", "session-probe", 0);
    assert_eq!(state.validate_inbound(&valid_after_mismatch), Ok(()));
    println!("session state OK: mismatch consumes neither message ID nor sequence");

    assert_eq!(
        state.validate_inbound(&valid_after_mismatch),
        Err(EnvelopeRejection::DuplicateMessageId {
            message_id: "message-shared".to_string(),
        })
    );

    let repeated_sequence = simple_envelope("message-sequence", "session-probe", 0);
    assert_eq!(
        state.validate_inbound(&repeated_sequence),
        Err(EnvelopeRejection::SequenceNotMonotonic {
            got: 0,
            last_seen: 0,
        })
    );

    let next = simple_envelope("message-next", "session-probe", 1);
    assert_eq!(state.validate_inbound(&next), Ok(()));

    let mut expired = simple_envelope("message-expired", "session-probe", 2);
    expired.expires_at = 0;
    assert!(matches!(
        state.validate_inbound(&expired),
        Err(EnvelopeRejection::Expired { .. })
    ));

    let mut standalone = InboundEnvelopeState::new();
    let arbitrary_session = simple_envelope("standalone", "any-session", 0);
    assert_eq!(standalone.validate_inbound(&arbitrary_session), Ok(()));
    println!("inbound validation OK: replay, sequence, expiry, and standalone cases passed");
}

fn probe_transport_signature_helpers() {
    let daemon_secret = SecretKey::generate();
    let daemon_public = daemon_secret.public();
    let client_secret = SecretKey::generate();
    let client_public = client_secret.public();

    let mut client_envelope = TaskEnvelope::<ClientMessage>::wrap(
        "transport-session".to_string(),
        Some("transport-task".to_string()),
        0,
        ClientMessage::Prompt {
            text: "signed client prompt".to_string(),
        },
    );
    let client_payload = client_envelope
        .signing_payload(
            EnvelopeDirection::ClientToDaemon,
            client_public.as_bytes(),
            daemon_public.as_bytes(),
        )
        .unwrap();
    client_envelope.signature = Some(encode_ed25519_signature(
        &client_secret.sign(&client_payload).to_bytes(),
    ));

    assert!(
        verify_client_envelope_for_probing(&client_envelope, &client_public, &daemon_public,)
            .is_ok()
    );
    assert!(
        verify_client_envelope_for_probing(&client_envelope, &client_public, &daemon_public,)
            .is_ok()
    );

    let mut missing = client_envelope.clone();
    missing.signature = None;
    assert_eq!(
        verify_client_envelope_for_probing(&missing, &client_public, &daemon_public),
        Err("signature is required".to_string())
    );

    let mut malformed = client_envelope.clone();
    malformed.signature = Some("ed25519:not-hex".to_string());
    assert!(
        verify_client_envelope_for_probing(&malformed, &client_public, &daemon_public)
            .unwrap_err()
            .contains("encoding")
    );

    let mut tampered = client_envelope.clone();
    tampered.payload = ClientMessage::Prompt {
        text: "tampered after signing".to_string(),
    };
    assert!(verify_client_envelope_for_probing(&tampered, &client_public, &daemon_public).is_err());
    assert!(
        verify_client_envelope_for_probing(
            &client_envelope,
            &SecretKey::generate().public(),
            &daemon_public,
        )
        .is_err()
    );
    assert!(
        verify_client_envelope_for_probing(
            &client_envelope,
            &client_public,
            &SecretKey::generate().public(),
        )
        .is_err()
    );

    let mut daemon_envelope = TaskEnvelope::<ServerMessage>::wrap(
        "transport-session".to_string(),
        Some("transport-task".to_string()),
        0,
        ServerMessage::ack(),
    );
    sign_daemon_envelope_for_probing(&mut daemon_envelope, &daemon_secret, &client_public).unwrap();
    let first_signature = daemon_envelope.signature.clone().unwrap();
    sign_daemon_envelope_for_probing(&mut daemon_envelope, &daemon_secret, &client_public).unwrap();
    assert_eq!(
        daemon_envelope.signature.as_deref(),
        Some(first_signature.as_str())
    );

    let signature_bytes = decode_ed25519_signature(&first_signature).unwrap();
    let signature = Signature::from_bytes(&signature_bytes);
    let daemon_payload = daemon_envelope
        .signing_payload(
            EnvelopeDirection::DaemonToClient,
            daemon_public.as_bytes(),
            client_public.as_bytes(),
        )
        .unwrap();
    daemon_public.verify(&daemon_payload, &signature).unwrap();

    let wrong_direction = daemon_envelope
        .signing_payload(
            EnvelopeDirection::ClientToDaemon,
            daemon_public.as_bytes(),
            client_public.as_bytes(),
        )
        .unwrap();
    assert!(daemon_public.verify(&wrong_direction, &signature).is_err());

    println!(
        "transport signatures OK: production daemon helpers are deterministic and reject missing, malformed, tampered, wrong-peer, wrong-recipient, and wrong-direction inputs"
    );
}

fn main() {
    probe_canonical_signing();
    probe_signature_codec();
    probe_session_and_replay_state();
    probe_transport_signature_helpers();
    println!("envelope_probe: ALL CHECKS PASSED");
}
