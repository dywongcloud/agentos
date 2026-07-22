//! Live witness for the escalate-and-take-control path: dials the running
//! daemon, starts a long text-only turn, sends `remote_control take_control`
//! and asserts the daemon PAUSES the agent (`task_active{paused:true}`), sends a
//! couple of input actions (move/click -- injected as real CGEvents on the Mac,
//! Accessibility permitting), then sends `release_control` and asserts the agent
//! RESUMES (`task_active{paused:false}`).
//!
//! Text-only prompt (no agent desktop actions). Requires a running daemon +
//! holo serve; NOT a headless CI probe.
//!   cargo run --example remote_control_live_probe -- <ticket> <pin>

use std::env;
use std::time::Duration;

use holoiroh_daemon::control_channel::{
    write_line, ClientMessage, RemoteControlEvent, ServerMessage, TaskEnvelope, CONTROL_ALPN,
};
use iroh::Endpoint;
use iroh_live::ticket::LiveTicket;
use tokio::io::{AsyncBufReadExt, BufReader};

const LONG_TEXT_PROMPT: &str = "Do not click, type, drag, open any app, or invoke ANY tool -- \
    writing only. Write a long, detailed essay (1500+ words) about the history of the bicycle, \
    slowly, section by section.";

async fn await_paused(
    lines: &mut tokio::io::Lines<BufReader<iroh::endpoint::RecvStream>>,
    want: bool,
    budget: Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + budget;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let Ok(Ok(Some(line))) = tokio::time::timeout(remaining, lines.next_line()).await else {
            break;
        };
        if let Ok(env) = serde_json::from_str::<TaskEnvelope<ServerMessage>>(&line) {
            match env.payload {
                ServerMessage::TaskActive { paused, .. } if paused == want => {
                    println!("  <- task_active {{ paused: {paused} }}");
                    return true;
                }
                ServerMessage::Status { text: Some(t) } => println!("  <- status: {t}"),
                _ => {}
            }
        }
    }
    false
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let mut args = env::args().skip(1);
    let ticket_str = args.next().expect("usage: remote_control_live_probe <ticket> <pin>");
    let pin = args.next().expect("usage: <pin>");
    let ticket: LiveTicket = ticket_str.parse()?;

    let endpoint = Endpoint::builder(iroh::endpoint::presets::N0).bind().await?;
    let conn = endpoint.connect(ticket.endpoint.clone(), CONTROL_ALPN).await?;
    let (mut send, recv) = conn.open_bi().await?;
    let mut lines = BufReader::new(recv).lines();
    write_line(&mut send, &ClientMessage::Pin { pin }).await?;

    let mut session_id: Option<String> = None;
    let start = tokio::time::Instant::now();
    while session_id.is_none() && start.elapsed() < Duration::from_secs(30) {
        if let Ok(Ok(Some(line))) = tokio::time::timeout(Duration::from_secs(30), lines.next_line()).await {
            if let Ok(env) = serde_json::from_str::<TaskEnvelope<ServerMessage>>(&line) {
                if matches!(&env.payload, ServerMessage::Status { text: Some(t) } if t.contains("control channel ready")) {
                    session_id = Some(env.session_id);
                }
            }
        }
    }
    let session_id = session_id.expect("no greeting within 30s");
    println!("session: {session_id}");

    // Start a long text-only turn and wait until it streams.
    let task_id = uuid::Uuid::new_v4().to_string();
    write_line(&mut send, &TaskEnvelope::<ClientMessage>::wrap(session_id.clone(), Some(task_id), 0, ClientMessage::Prompt { text: LONG_TEXT_PROMPT.into() })).await?;
    println!("-> long text prompt; waiting for it to stream...");
    let live_start = tokio::time::Instant::now();
    let mut streaming = false;
    while !streaming && live_start.elapsed() < Duration::from_secs(120) {
        if let Ok(Ok(Some(line))) = tokio::time::timeout(Duration::from_secs(120), lines.next_line()).await {
            if let Ok(env) = serde_json::from_str::<TaskEnvelope<ServerMessage>>(&line) {
                if matches!(env.payload, ServerMessage::TaskProgress { .. }) {
                    streaming = true;
                }
            }
        }
    }
    assert!(streaming, "turn never streamed");
    println!("turn is live. Sending remote_control take_control (escalate)...");

    // The prompt above used sequence 0; every subsequent client envelope MUST
    // use a strictly increasing sequence or the daemon's envelope validation
    // drops it. (This bit an earlier version of this probe.)
    let mut seq: u64 = 1;
    let rc = |ev: RemoteControlEvent, s: u64| {
        TaskEnvelope::<ClientMessage>::wrap(session_id.clone(), None, s, ClientMessage::RemoteControl { event: ev })
    };
    write_line(&mut send, &rc(RemoteControlEvent::TakeControl, seq)).await?;
    seq += 1;
    let paused = await_paused(&mut lines, true, Duration::from_secs(10)).await;

    // While in control, send a couple of input actions (injected on the Mac).
    write_line(&mut send, &rc(RemoteControlEvent::Move { x: 0.5, y: 0.5 }, seq)).await?;
    seq += 1;
    write_line(&mut send, &rc(RemoteControlEvent::Click { x: 0.5, y: 0.5, button: holoiroh_daemon::control_channel::MouseButton::Left, count: 1 }, seq)).await?;
    seq += 1;
    println!("(sent move + click while in control)");
    tokio::time::sleep(Duration::from_secs(1)).await;

    println!("Sending release_control...");
    write_line(&mut send, &rc(RemoteControlEvent::ReleaseControl, seq)).await?;
    let resumed = await_paused(&mut lines, false, Duration::from_secs(15)).await;

    println!();
    println!("=== RESULT ===");
    if paused && resumed {
        println!("VERDICT: OK -- take_control paused the agent, release_control resumed it (input actions sent in between).");
    } else {
        println!("VERDICT: BROKEN -- pause_seen={paused} resume_seen={resumed}");
        std::process::exit(1);
    }
    Ok(())
}
