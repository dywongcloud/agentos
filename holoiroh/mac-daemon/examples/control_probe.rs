//! Manual, run-by-hand probe: dials a running holoiroh-daemon's control
//! channel over the real iroh transport and exchanges real envelope-wrapped
//! ClientMessage / ServerMessage JSON, to witness control_channel.rs
//! end-to-end (not just its serde unit tests). Not part of the crate's
//! normal build/test surface -- run explicitly with `cargo run --example
//! control_probe -- <ticket> [pin]`.
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
//!
//! ## PIN handshake stays unwrapped; everything after it is envelope-wrapped
//!
//! Per `PROTOCOL.md`'s "Envelope" section, the `pin`/`auth_rejected`
//! exchange happens *before* a `session_id` exists (a `session_id` is only
//! minted once the auth gate passes -- see `control_channel.rs`'s
//! `ControlChannel::accept`), so those two messages are sent/received as
//! bare `ClientMessage`/`ServerMessage` JSON, exactly as before this task's
//! envelope wrapping. Every message from the "control channel ready"
//! greeting onward is a real `TaskEnvelope<ServerMessage>` /
//! `TaskEnvelope<ClientMessage>` on the wire, which this probe constructs
//! and parses for real (not simulated) to witness the actual daemon
//! behavior over a live connection.
use std::env;

use holoiroh_daemon::control_channel::{
    write_line, ClientMessage, ServerMessage, TaskEnvelope, CONTROL_ALPN,
};
use iroh::Endpoint;
use iroh_live::ticket::LiveTicket;
use tokio::io::{AsyncBufReadExt, BufReader};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let mut args = env::args().skip(1);
    let ticket_str = args.next().expect("usage: control_probe <ticket> [pin] [prompt]");
    let pin = args.next();
    // Optional third arg: the prompt text to send after auth. Defaults to the
    // original harmless hello. A real task here streams a REAL agent turn on
    // the daemon's desktop -- pass explicit no-op instructions when the goal
    // is witnessing the transport/turn plumbing rather than desktop actions.
    let prompt_text = args
        .next()
        .unwrap_or_else(|| "control_probe: hello from a real iroh dial (post-PIN)".to_string());
    let ticket: LiveTicket = ticket_str.parse()?;

    let endpoint = Endpoint::builder(iroh::endpoint::presets::N0).bind().await?;
    let conn = endpoint.connect(ticket.endpoint.clone(), CONTROL_ALPN).await?;
    println!("connected: remote={}", conn.remote_id().fmt_short());

    let (mut send, recv) = conn.open_bi().await?;
    let mut lines = BufReader::new(recv).lines();

    if let Some(pin) = pin {
        // PIN provided: present it first, per the auth gate's contract
        // (the first line from an unrecognized device must be a Pin
        // message before anything else is processed). Bare, not
        // envelope-wrapped -- see this file's module doc.
        let pin_msg = ClientMessage::Pin { pin };
        write_line(&mut send, &pin_msg).await?;
        println!("-> {}", serde_json::to_string(&pin_msg)?);

        // On success, the next line is the normal greeting -- now a real
        // TaskEnvelope<ServerMessage>, carrying this connection's minted
        // session_id.
        let greeting = lines.next_line().await?.expect("no greeting received after PIN");
        println!("<- {greeting}");
        let greeting_env: TaskEnvelope<ServerMessage> = serde_json::from_str(&greeting)?;
        assert!(
            matches!(greeting_env.payload, ServerMessage::Status { .. }),
            "expected greeting after correct PIN, got {:?}",
            greeting_env.payload
        );
        assert_eq!(greeting_env.message_type, "status");
        assert_eq!(greeting_env.sequence_number, 0, "greeting must be this connection's first outbound envelope");
        let session_id = greeting_env.session_id.clone();
        println!(
            "   (envelope: session_id={} message_id={} sequence_number={})",
            greeting_env.session_id, greeting_env.message_id, greeting_env.sequence_number
        );

        // Every ClientMessage from here on is wrapped in a real
        // TaskEnvelope<ClientMessage>, with an inbound sequence_number this
        // probe tracks and increments itself (mirroring what a real client
        // implementation must do to satisfy the daemon's monotonicity
        // check).
        let task_id = uuid::Uuid::new_v4().to_string();
        let prompt_env = TaskEnvelope::<ClientMessage>::wrap(
            session_id.clone(),
            Some(task_id.clone()),
            0,
            ClientMessage::Prompt {
                text: prompt_text,
            },
        );
        write_line(&mut send, &prompt_env).await?;
        println!("-> {}", serde_json::to_string(&prompt_env)?);

        let ack = lines.next_line().await?.expect("no ack received");
        println!("<- {ack}");
        let ack_env: TaskEnvelope<ServerMessage> = serde_json::from_str(&ack)?;
        assert!(
            matches!(ack_env.payload, ServerMessage::Ack { .. }),
            "expected ack, got {:?}",
            ack_env.payload
        );
        assert_eq!(ack_env.session_id, session_id, "ack must echo this connection's session_id");
        assert_eq!(
            ack_env.task_id,
            Some(task_id.clone()),
            "ack must echo the prompt's task_id"
        );
        println!(
            "   (envelope: session_id={} task_id={:?} sequence_number={})",
            ack_env.session_id, ack_env.task_id, ack_env.sequence_number
        );

        // Stream the turn to its end: every post-ack ServerMessage, until a
        // `status` line arrives after at least one `task_progress` (Done maps
        // to `status` on the wire -- see control_channel.rs's
        // from_control_event; there is no distinct terminal type), or the
        // overall/idle timeouts land. This turns the probe into a full
        // phone-equivalent end-to-end witness of a live daemon turn.
        let overall = tokio::time::Instant::now();
        let mut saw_progress = false;
        loop {
            if overall.elapsed() > std::time::Duration::from_secs(600) {
                println!("control_probe: overall turn timeout (600s)");
                break;
            }
            match tokio::time::timeout(std::time::Duration::from_secs(120), lines.next_line()).await {
                Err(_) => {
                    println!("control_probe: idle timeout (120s with no server line)");
                    break;
                }
                Ok(Ok(None)) => {
                    println!("control_probe: stream closed by daemon");
                    break;
                }
                Ok(Err(err)) => {
                    println!("control_probe: read error: {err}");
                    break;
                }
                Ok(Ok(Some(line))) => {
                    println!("<- {line}");
                    if let Ok(env) = serde_json::from_str::<TaskEnvelope<ServerMessage>>(&line) {
                        match env.payload {
                            ServerMessage::TaskProgress { .. } => saw_progress = true,
                            ServerMessage::Status { text } => {
                                let queued = text.as_deref().map(|t| t.starts_with("queued")).unwrap_or(false);
                                if saw_progress && !queued {
                                    println!("control_probe: terminal status after progress -- turn concluded");
                                    break;
                                }
                            }
                            ServerMessage::Error { .. } => {
                                println!("control_probe: error message received");
                                break;
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        println!("control_probe: OK -- PIN accepted, envelope-wrapped greeting + ack witnessed over a real iroh connection");
    } else {
        // No PIN provided: an unrecognized device must be rejected before
        // the greeting is ever sent. This is the real rejection path
        // witnessed against the daemon's actual `iroh` transport, not a
        // unit test standing in for it.
        //
        // The daemon's `authenticate` gate blocks on reading a line before
        // sending anything -- so the greeting genuinely never arrives.
        // Send a bare Prompt (simulating a client that ignores the PIN
        // requirement) so the gate has something to read and reject. Bare,
        // not envelope-wrapped: no session_id exists yet at this point on
        // either side (see this file's module doc), and the daemon's
        // `authenticate` gate itself only ever expects a bare
        // `ClientMessage` on this pre-session path regardless of what's
        // sent -- an envelope-wrapped payload here would just be a
        // different flavor of "not a Pin message", rejected the same way.
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
                println!("control_probe: OK -- unrecognized device correctly rejected (auth_rejected, bare -- pre-session)");
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
