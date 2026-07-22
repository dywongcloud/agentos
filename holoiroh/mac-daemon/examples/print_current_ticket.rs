//! Derive the RUNNING daemon's current pairing ticket from its stable identity
//! key (`~/.holoiroh/iroh_secret`) WITHOUT binding a second endpoint under that
//! same identity (which would fight the live daemon for its pkarr record). The
//! ticket is node-id-only: the iOS/client endpoint resolves the node id to the
//! daemon's current relay + direct paths via N0 discovery (the daemon publishes
//! that record on bind), so there are no stale address hints to drift -- exactly
//! the "ticket already stable via iroh_secret" property `main.rs` relies on.
//!
//! Then, as a live witness, it dials the derived ticket with a FRESH client
//! identity (no conflict) + the stable pairing PIN and waits for the daemon's
//! "control channel ready" greeting -- proving the derived ticket actually
//! reaches the running daemon, so it is safe to bake into the iOS app's seeded
//! "Dev Mac" default profile (`ConnectionProfileStore.currentDevTicket`).
//!
//!   cargo run --example print_current_ticket [-- <pin>]   (pin defaults to 394299)

use std::time::Duration;

use holoiroh_daemon::control_channel::{
    write_line, ClientMessage, ServerMessage, TaskEnvelope, CONTROL_ALPN,
};
use iroh::{Endpoint, EndpointAddr, SecretKey};
use iroh_live::ticket::LiveTicket;
use tokio::io::{AsyncBufReadExt, BufReader};

const BROADCAST_NAME: &str = "holoiroh";

fn derive_ticket() -> anyhow::Result<(LiveTicket, String)> {
    let home = std::env::var("HOME").map_err(|_| anyhow::anyhow!("HOME not set"))?;
    let path = format!("{home}/.holoiroh/iroh_secret");
    let hex = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("reading {path}: {e}"))?;
    let secret: SecretKey = hex
        .trim()
        .parse()
        .map_err(|e| anyhow::anyhow!("parsing iroh_secret: {e}"))?;
    let endpoint_id = secret.public();
    let addr = EndpointAddr::from(endpoint_id);
    Ok((LiveTicket::new(addr, BROADCAST_NAME), endpoint_id.to_string()))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let pin = std::env::args().nth(1).unwrap_or_else(|| "394299".to_string());

    let (ticket, endpoint_id) = derive_ticket()?;
    let ticket_str = ticket.to_string();
    println!("ENDPOINT_ID={endpoint_id}");
    println!("TICKET={ticket_str}");

    // Live witness: dial the derived ticket with a fresh client identity.
    let endpoint = Endpoint::builder(iroh::endpoint::presets::N0).bind().await?;
    let conn = endpoint.connect(ticket.endpoint.clone(), CONTROL_ALPN).await?;
    let (mut send, recv) = conn.open_bi().await?;
    let mut lines = BufReader::new(recv).lines();
    write_line(&mut send, &ClientMessage::Pin { pin }).await?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut ready = false;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let Ok(Ok(Some(line))) = tokio::time::timeout(remaining, lines.next_line()).await else {
            break;
        };
        if let Ok(env) = serde_json::from_str::<TaskEnvelope<ServerMessage>>(&line) {
            if let ServerMessage::Status { text: Some(t) } = env.payload {
                println!("  <- status: {t}");
                if t.contains("control channel ready") {
                    ready = true;
                    break;
                }
            }
        }
    }

    println!();
    if ready {
        println!("CONNECT_OK -- the derived ticket reaches the running daemon.");
    } else {
        println!("CONNECT_FAIL -- no 'control channel ready' greeting within 30s.");
        std::process::exit(1);
    }
    Ok(())
}
