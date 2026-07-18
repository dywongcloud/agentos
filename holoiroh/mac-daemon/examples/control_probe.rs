//! Manual, run-by-hand probe: dials a running holoiroh-daemon's control
//! channel over the real iroh transport and exchanges real ClientMessage /
//! ServerMessage JSON, to witness control_channel.rs end-to-end (not just
//! its serde unit tests). Not part of the crate's normal build/test surface
//! -- run explicitly with `cargo run --example control_probe -- <ticket> [pin]`.
//!
//! With no `[pin]` argument, this probes the **rejection** path: the daemon
//! (started without `--no-pin-auth`) requires a PIN from an unrecognized
//! device (see `control_channel.rs::ControlChannel::authenticate` and
//! `holoiroh/PAIRING.md`) before sending the "control channel ready"
//! greeting at all, so a bare dial with no PIN gets an `auth_rejected`
//! message and a closed connection instead. Pass the PIN the daemon printed
//! at startup as a second argument to probe the **accept** path instead
//! (greeting + prompt/ack, as this probe originally exercised before PIN
//! auth existed).
use std::env;

use holoiroh_daemon::control_channel::{write_line, ClientMessage, ServerMessage, CONTROL_ALPN};
use iroh::Endpoint;
use iroh_live::ticket::LiveTicket;
use tokio::io::{AsyncBufReadExt, BufReader};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let mut args = env::args().skip(1);
    let ticket_str = args.next().expect("usage: control_probe <ticket> [pin]");
    let pin = args.next();
    let ticket: LiveTicket = ticket_str.parse()?;

    let endpoint = Endpoint::builder(iroh::endpoint::presets::N0).bind().await?;
    let conn = endpoint.connect(ticket.endpoint.clone(), CONTROL_ALPN).await?;
    println!("connected: remote={}", conn.remote_id().fmt_short());

    let (mut send, recv) = conn.open_bi().await?;
    let mut lines = BufReader::new(recv).lines();

    if let Some(pin) = pin {
        // PIN provided: present it first, per the auth gate's contract
        // (the first line from an unrecognized device must be a Pin
        // message before anything else is processed).
        let pin_msg = ClientMessage::Pin { pin };
        write_line(&mut send, &pin_msg).await?;
        println!("-> {}", serde_json::to_string(&pin_msg)?);

        // On success, the next line is the normal greeting.
        let greeting = lines.next_line().await?.expect("no greeting received after PIN");
        println!("<- {greeting}");
        let greeting_msg: ServerMessage = serde_json::from_str(&greeting)?;
        assert!(
            matches!(greeting_msg, ServerMessage::Status { .. }),
            "expected greeting after correct PIN, got {greeting_msg:?}"
        );

        let prompt = ClientMessage::Prompt {
            text: "control_probe: hello from a real iroh dial (post-PIN)".to_string(),
        };
        write_line(&mut send, &prompt).await?;
        println!("-> {}", serde_json::to_string(&prompt)?);

        let ack = lines.next_line().await?.expect("no ack received");
        println!("<- {ack}");
        let ack_msg: ServerMessage = serde_json::from_str(&ack)?;
        assert!(matches!(ack_msg, ServerMessage::Ack { .. }), "expected ack, got {ack_msg:?}");

        println!("control_probe: OK -- PIN accepted, greeting + ack witnessed over a real iroh connection");
    } else {
        // No PIN provided: an unrecognized device must be rejected before
        // the greeting is ever sent. This is the real rejection path
        // witnessed against the daemon's actual `iroh` transport, not a
        // unit test standing in for it.
        //
        // The daemon's `authenticate` gate blocks on reading a line before
        // sending anything -- so the greeting genuinely never arrives.
        // Send a bare Prompt (simulating a client that ignores the PIN
        // requirement) so the gate has something to read and reject.
        let prompt = ClientMessage::Prompt {
            text: "control_probe: attempting without a PIN".to_string(),
        };
        write_line(&mut send, &prompt).await?;
        println!("-> {} (no PIN presented)", serde_json::to_string(&prompt)?);

        let response = lines.next_line().await?;
        match response {
            Some(line) => {
                println!("<- {line}");
                let msg: ServerMessage = serde_json::from_str(&line)?;
                assert!(
                    matches!(msg, ServerMessage::AuthRejected { .. }),
                    "expected auth_rejected for an unrecognized device with no PIN, got {msg:?}"
                );
                println!("control_probe: OK -- unrecognized device correctly rejected (auth_rejected)");
            }
            None => {
                // Some transports may observe rejection as a clean close
                // with no readable line at all, depending on timing --
                // still a correct rejection (no greeting/ack was ever
                // obtained), just via a different observable.
                println!("control_probe: OK -- connection closed with no greeting (rejected before any message)");
            }
        }
    }

    Ok(())
}
