//! Live witness for daemon-ticket-broadcast: dials the RUNNING daemon with a
//! fresh client identity + the pairing PIN and confirms it sends a
//! `CurrentTicket` right after the greeting, carrying the daemon's own node-id
//! ticket. Local live probe (needs a running daemon on the same network), so
//! deliberately NOT in CI -- same status as `control_ffi_probe`.
//!
//!   cargo run --example current_ticket_live_probe -p holoiroh-daemon [-- <pin>]

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
    let expected = ticket.to_string();
    println!("EXPECTED_TICKET={expected}");

    let endpoint = Endpoint::builder(iroh::endpoint::presets::N0).bind().await?;
    let conn = endpoint.connect(ticket.endpoint.clone(), CONTROL_ALPN).await?;
    let (mut send, recv) = conn.open_bi().await?;
    let mut lines = BufReader::new(recv).lines();
    write_line(&mut send, &ClientMessage::Pin { pin }).await?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut got_greeting = false;
    let mut got_ticket: Option<String> = None;
    while tokio::time::Instant::now() < deadline && got_ticket.is_none() {
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
            Some(ServerMessage::Status { text }) if text.as_deref() == Some("control channel ready") => {
                got_greeting = true;
                println!("greeting: control channel ready");
            }
            Some(ServerMessage::CurrentTicket { ticket }) => {
                println!("received current_ticket: {ticket}");
                got_ticket = Some(ticket);
            }
            Some(other) => println!("(other: {})", other.type_tag()),
            None => println!("(unparsed: {line})"),
        }
    }

    let received = got_ticket.ok_or_else(|| anyhow::anyhow!("no current_ticket received within 20s"))?;
    assert!(got_greeting, "greeting must arrive before current_ticket");
    assert_eq!(received, expected, "current_ticket must equal the daemon's node-id ticket");
    println!("current_ticket_live_probe: OK -- daemon broadcast CurrentTicket = {received}");
    Ok(())
}
