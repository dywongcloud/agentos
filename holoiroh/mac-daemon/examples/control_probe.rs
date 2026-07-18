//! Manual, run-by-hand probe: dials a running holoiroh-daemon's control
//! channel over the real iroh transport and exchanges real ClientMessage /
//! ServerMessage JSON, to witness control_channel.rs end-to-end (not just
//! its serde unit tests). Not part of the crate's normal build/test surface
//! -- run explicitly with `cargo run --example control_probe -- <ticket>`.
use std::env;

use holoiroh_daemon::control_channel::{write_line, ClientMessage, ServerMessage, CONTROL_ALPN};
use iroh::Endpoint;
use iroh_live::ticket::LiveTicket;
use tokio::io::{AsyncBufReadExt, BufReader};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let ticket_str = env::args().nth(1).expect("usage: control_probe <ticket>");
    let ticket: LiveTicket = ticket_str.parse()?;

    let endpoint = Endpoint::builder(iroh::endpoint::presets::N0).bind().await?;
    let conn = endpoint.connect(ticket.endpoint.clone(), CONTROL_ALPN).await?;
    println!("connected: remote={}", conn.remote_id().fmt_short());

    let (mut send, recv) = conn.open_bi().await?;
    let mut lines = BufReader::new(recv).lines();

    // Expect the greeting ServerMessage::status("control channel ready").
    let greeting = lines.next_line().await?.expect("no greeting received");
    println!("<- {greeting}");
    let greeting_msg: ServerMessage = serde_json::from_str(&greeting)?;
    assert!(matches!(greeting_msg, ServerMessage::Status { .. }));

    // Send a real ClientMessage::Prompt and read back the ack.
    let prompt = ClientMessage::Prompt {
        text: "control_probe: hello from a real iroh dial".to_string(),
    };
    write_line(&mut send, &prompt).await?;
    println!("-> {}", serde_json::to_string(&prompt)?);

    let ack = lines.next_line().await?.expect("no ack received");
    println!("<- {ack}");
    let ack_msg: ServerMessage = serde_json::from_str(&ack)?;
    assert!(matches!(ack_msg, ServerMessage::Ack { .. }), "expected ack, got {ack_msg:?}");

    println!("control_probe: OK -- greeting + ack witnessed over a real iroh connection");
    Ok(())
}
