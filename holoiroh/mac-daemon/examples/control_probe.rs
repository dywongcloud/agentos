//! External transport probe for signed control envelopes and tamper rejection.
//!
//! Run against a real daemon:
//! `cargo run --example control_probe -- <ticket> <pin> [prompt]`.

use std::env;

use holoiroh_daemon::control_channel::{
    CONTROL_ALPN, ClientMessage, EnvelopeDirection, InboundEnvelopeState, PROTOCOL_VERSION,
    ServerMessage, TaskEnvelope, decode_ed25519_signature, encode_ed25519_signature, write_line,
};
use iroh::{Endpoint, PublicKey, SecretKey, Signature};
use iroh_live::ticket::LiveTicket;
use tokio::io::{AsyncBufReadExt, BufReader, Lines};

fn sign_client(
    envelope: &mut TaskEnvelope<ClientMessage>,
    signer: &SecretKey,
    recipient: &PublicKey,
) {
    let public = signer.public();
    let payload = envelope
        .signing_payload(
            EnvelopeDirection::ClientToDaemon,
            public.as_bytes(),
            recipient.as_bytes(),
        )
        .unwrap();
    envelope.signature = Some(encode_ed25519_signature(&signer.sign(&payload).to_bytes()));
}

fn verify_server(
    line: &str,
    signer: &PublicKey,
    recipient: &PublicKey,
    state: &mut InboundEnvelopeState,
) -> TaskEnvelope<ServerMessage> {
    let shell: TaskEnvelope<serde_json::Value> = serde_json::from_str(line).unwrap();
    assert_eq!(shell.protocol_version, PROTOCOL_VERSION);
    let encoded = shell
        .signature
        .as_deref()
        .expect("server signature missing");
    let bytes = decode_ed25519_signature(encoded).expect("server signature encoding invalid");
    let signature = Signature::from_bytes(&bytes);
    let payload = shell
        .signing_payload(
            EnvelopeDirection::DaemonToClient,
            signer.as_bytes(),
            recipient.as_bytes(),
        )
        .unwrap();
    signer
        .verify(&payload, &signature)
        .expect("server signature verification failed");
    let typed: ServerMessage = serde_json::from_value(shell.payload.clone()).unwrap();
    assert_eq!(shell.message_type, typed.type_tag());
    state.validate_inbound(&shell).unwrap();
    TaskEnvelope {
        protocol_version: shell.protocol_version,
        message_id: shell.message_id,
        session_id: shell.session_id,
        task_id: shell.task_id,
        message_type: shell.message_type,
        sent_at: shell.sent_at,
        expires_at: shell.expires_at,
        sequence_number: shell.sequence_number,
        payload: typed,
        signature: shell.signature,
    }
}

async fn expect_signed_rejection<R>(
    lines: &mut Lines<R>,
    daemon: &PublicKey,
    client: &PublicKey,
    state: &mut InboundEnvelopeState,
    label: &str,
) where
    R: tokio::io::AsyncBufRead + Unpin,
{
    loop {
        let line = tokio::time::timeout(std::time::Duration::from_secs(10), lines.next_line())
            .await
            .unwrap_or_else(|_| panic!("timeout waiting for {label} rejection"))
            .unwrap()
            .unwrap_or_else(|| panic!("stream closed waiting for {label} rejection"));
        let envelope = verify_server(&line, daemon, client, state);
        match envelope.payload {
            ServerMessage::Error { .. } => {
                assert!(envelope.task_id.is_none());
                println!("rejected: {label}");
                return;
            }
            ServerMessage::CurrentTicket { .. } | ServerMessage::TinfoilVerification { .. } => {}
            ServerMessage::Ack { .. }
            | ServerMessage::TaskProgress { .. }
            | ServerMessage::TaskDone { .. } => {
                panic!("{label} reached task dispatch: {:?}", envelope.payload)
            }
            _ => {}
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let mut args = env::args().skip(1);
    let ticket_str = args
        .next()
        .expect("usage: control_probe <ticket> [pin] [prompt]");
    let pin = args.next();
    let prompt_text = args
        .next()
        .unwrap_or_else(|| "control_probe signed baseline".to_string());
    let ticket: LiveTicket = ticket_str.parse()?;

    let endpoint = Endpoint::builder(iroh::endpoint::presets::N0)
        .bind()
        .await?;
    let conn = endpoint
        .connect(ticket.endpoint.clone(), CONTROL_ALPN)
        .await?;
    let daemon_public = conn.remote_id();
    let client_secret = endpoint.secret_key();
    let client_public = client_secret.public();
    println!("connected: remote={}", daemon_public.fmt_short());

    let (mut send, recv) = conn.open_bi().await?;
    let mut lines = BufReader::new(recv).lines();

    let Some(pin) = pin else {
        let prompt = ClientMessage::Prompt {
            text: "control_probe: attempting without a PIN".to_string(),
        };
        write_line(&mut send, &prompt).await?;
        let response = lines.next_line().await?;
        if let Some(line) = response {
            let message: ServerMessage = serde_json::from_str(&line)?;
            assert!(matches!(message, ServerMessage::AuthRejected { .. }));
        }
        println!("control_probe: unauthenticated connection rejected");
        return Ok(());
    };

    write_line(&mut send, &ClientMessage::Pin { pin }).await?;
    let greeting = lines.next_line().await?.expect("no greeting after PIN");
    let greeting_shell: TaskEnvelope<serde_json::Value> = serde_json::from_str(&greeting)?;
    assert!(!greeting_shell.session_id.is_empty());
    assert_eq!(greeting_shell.sequence_number, 0);
    let session_id = greeting_shell.session_id.clone();
    let mut server_state = InboundEnvelopeState::for_session(session_id.clone());
    let greeting = verify_server(&greeting, &daemon_public, &client_public, &mut server_state);
    assert!(matches!(
        greeting.payload,
        ServerMessage::Status { ref text, .. }
            if text.as_deref() == Some("control channel ready")
    ));

    let task_id = uuid::Uuid::new_v4().to_string();
    let mut baseline = TaskEnvelope::<ClientMessage>::wrap(
        session_id.clone(),
        Some(task_id.clone()),
        0,
        ClientMessage::Prompt { text: prompt_text },
    );
    sign_client(&mut baseline, client_secret, &daemon_public);
    println!("baseline signed client envelope constructed at seq0");

    let mut cases: Vec<(&str, TaskEnvelope<ClientMessage>)> = Vec::new();

    let mut tampered = baseline.clone();
    tampered.payload = ClientMessage::Prompt {
        text: "tampered payload".to_string(),
    };
    cases.push(("payload changed after signing", tampered));

    let mut tampered = baseline.clone();
    tampered.session_id.push_str("-tampered");
    cases.push(("session changed after signing", tampered));

    let mut tampered = baseline.clone();
    tampered.sequence_number = 99;
    cases.push(("sequence changed after signing", tampered));

    let mut tampered = baseline.clone();
    tampered.expires_at = 0;
    cases.push(("expiry changed after signing", tampered));

    let mut tampered = baseline.clone();
    tampered.message_id.push_str("-tampered");
    cases.push(("message_id changed after signing", tampered));

    let mut tampered = baseline.clone();
    let signature = tampered.signature.as_mut().unwrap();
    let replacement = if signature.ends_with('0') { '1' } else { '0' };
    signature.pop();
    signature.push(replacement);
    cases.push(("signature bytes tampered", tampered));

    let wrong_key = SecretKey::generate();
    let mut tampered = baseline.clone();
    tampered.signature = None;
    sign_client(&mut tampered, &wrong_key, &daemon_public);
    cases.push(("wrong transport-peer signing key", tampered));

    let mut tampered = baseline.clone();
    tampered.signature = None;
    cases.push(("missing signature", tampered));

    let mut tampered = baseline.clone();
    let encoded = tampered.signature.take().unwrap();
    tampered.signature = Some(format!("ed25519:{}", encoded[8..].to_ascii_uppercase()));
    cases.push(("uppercase signature", tampered));

    let mut tampered = baseline.clone();
    tampered.signature = Some(format!("ed25519:{}", "zz".repeat(64)));
    cases.push(("malformed signature", tampered));

    let mut wrong_type = baseline.clone();
    wrong_type.message_type = "stop".to_string();
    wrong_type.signature = None;
    sign_client(&mut wrong_type, client_secret, &daemon_public);
    cases.push(("valid signature but message_type mismatch", wrong_type));

    let mut wrong_session = baseline.clone();
    wrong_session.session_id = "different-validly-signed-session".to_string();
    wrong_session.signature = None;
    sign_client(&mut wrong_session, client_secret, &daemon_public);
    cases.push(("valid signature but session mismatch", wrong_session));

    for (label, envelope) in cases {
        write_line(&mut send, &envelope).await?;
        expect_signed_rejection(
            &mut lines,
            &daemon_public,
            &client_public,
            &mut server_state,
            label,
        )
        .await;
    }

    write_line(&mut send, &baseline).await?;
    loop {
        let line = tokio::time::timeout(std::time::Duration::from_secs(10), lines.next_line())
            .await??
            .expect("stream closed before final valid seq0 ack");
        let envelope = verify_server(&line, &daemon_public, &client_public, &mut server_state);
        match envelope.payload {
            ServerMessage::Ack { .. } => {
                assert_eq!(envelope.task_id, Some(task_id));
                break;
            }
            ServerMessage::CurrentTicket { .. } | ServerMessage::TinfoilVerification { .. } => {}
            ServerMessage::Error { .. } => panic!("final valid seq0 was rejected"),
            _ => {}
        }
    }

    println!(
        "control_probe: ALL SIGNATURE TAMPERS REJECTED; final original signed seq0 dispatched"
    );
    Ok(())
}
