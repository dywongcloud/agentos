//! Live end-to-end witness for the clarify HANDLER: dials the running daemon
//! with a fresh client identity + PIN, sends an envelope-wrapped ClarifyRequest
//! for an ambiguous prompt, and confirms a ClarifyQuestions comes back with real
//! options -- proving the control channel handles clarification OFF the task
//! pipeline. Local live probe (needs the running daemon + its Tinfoil key), not CI.
//!
//!   cargo run --example clarify_live_probe -p holoiroh-daemon [-- <pin>]

use std::time::Duration;

use holoiroh_daemon::control_channel::{
    CONTROL_ALPN, ClientMessage, ServerMessage, TaskEnvelope, write_line,
};
use iroh::{Endpoint, EndpointAddr, SecretKey};
use iroh_live::ticket::LiveTicket;
use tokio::io::{AsyncBufReadExt, BufReader};

const BROADCAST_NAME: &str = "holoiroh";

fn derive_ticket() -> anyhow::Result<LiveTicket> {
    let home = std::env::var("HOME").map_err(|_| anyhow::anyhow!("HOME not set"))?;
    let hex = std::fs::read_to_string(format!("{home}/.holoiroh/iroh_secret"))?;
    let secret: SecretKey = hex.trim().parse().map_err(|e| anyhow::anyhow!("parsing secret: {e}"))?;
    Ok(LiveTicket::new(EndpointAddr::from(secret.public()), BROADCAST_NAME))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let pin = std::env::args().nth(1).unwrap_or_else(|| "394299".to_string());
    let ticket = derive_ticket()?;

    let endpoint = Endpoint::builder(iroh::endpoint::presets::N0).bind().await?;
    let conn = endpoint.connect(ticket.endpoint.clone(), CONTROL_ALPN).await?;
    let (mut send, recv) = conn.open_bi().await?;
    let mut lines = BufReader::new(recv).lines();
    write_line(&mut send, &ClientMessage::Pin { pin }).await?;

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock before epoch")
        .as_millis() as u64;
    let envelope = serde_json::json!({
        "protocol_version": 1,
        "message_id": format!("clarify-probe-{now_ms}"),
        "session_id": "clarify-live-probe",
        "message_type": "clarify_request",
        "sent_at": now_ms,
        "expires_at": now_ms + 30_000,
        "sequence_number": 0,
        "payload": {"type": "clarify_request", "prompt": "send a message to the team about the meeting"},
    });
    write_line(&mut send, &envelope).await?;
    println!("sent ClarifyRequest");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut questions: Option<Vec<holoiroh_wire::ClarifyingQuestion>> = None;
    while tokio::time::Instant::now() < deadline && questions.is_none() {
        let remaining = deadline - tokio::time::Instant::now();
        let line = match tokio::time::timeout(remaining, lines.next_line()).await {
            Ok(Ok(Some(l))) => l,
            _ => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let msg: Option<ServerMessage> = serde_json::from_str::<TaskEnvelope<ServerMessage>>(&line)
            .map(|e| e.payload)
            .ok()
            .or_else(|| serde_json::from_str::<ServerMessage>(&line).ok());
        match msg {
            Some(ServerMessage::ClarifyQuestions { questions: qs }) => {
                println!("received ClarifyQuestions: {} question(s)", qs.len());
                for q in &qs {
                    println!("  Q: {} | options: {:?}", q.question, q.options);
                }
                questions = Some(qs);
            }
            Some(other) => println!("(other: {})", other.type_tag()),
            None => println!("(unparsed: {line})"),
        }
    }

    let qs = questions.ok_or_else(|| anyhow::anyhow!("no ClarifyQuestions received within 30s"))?;
    assert!(!qs.is_empty(), "ambiguous prompt must yield questions from the live handler");
    println!("clarify_live_probe: OK -- live daemon answered ClarifyRequest with {} clarifying question(s)", qs.len());
    Ok(())
}
