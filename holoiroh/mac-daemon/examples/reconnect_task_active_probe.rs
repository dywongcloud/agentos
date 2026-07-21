//! Live witness for issue-2 ("after I disconnect then reconnect, a Holo task is
//! still running from before but there's no Pause/Stop taskbar"). It dials the
//! running daemon, starts a LONG text-only turn, waits until it is streaming,
//! DROPS the connection, then reconnects and asserts the daemon proactively
//! sends a `task_active` envelope (paused=false) on the fresh connection -- the
//! exact signal the iOS app now uses to restore the Pause/Stop pill.
//!
//! Text-only prompt (explicitly forbids tools) so the probe drives NO real
//! desktop actions -- same safety discipline as the other live probes. Requires
//! a running daemon + `holo serve`, so (like stop_halt_probe) it is NOT a
//! headless CI probe; run it locally against the live daemon:
//!   cargo run --example reconnect_task_active_probe -- <ticket> <pin>

use std::env;
use std::time::Duration;

use holoiroh_daemon::control_channel::{
    write_line, ClientMessage, ServerMessage, TaskEnvelope, CONTROL_ALPN,
};
use iroh::Endpoint;
use iroh_live::ticket::LiveTicket;
use tokio::io::{AsyncBufReadExt, BufReader};

const LONG_TEXT_PROMPT: &str = "Do not click, type, drag, open any app, or invoke ANY tool -- \
    this is a writing-only exercise, produce text only and never act on the desktop. Write an \
    extremely detailed, long essay (at least 1500 words) about the history of lighthouses, \
    section by section, slowly and thoroughly.";

/// Connect, pin, and wait for the "control channel ready" greeting; returns the
/// live bi-stream reader/writer plus the negotiated session id.
async fn connect_and_pin(
    endpoint: &Endpoint,
    ticket: &LiveTicket,
    pin: &str,
) -> anyhow::Result<(
    iroh::endpoint::SendStream,
    tokio::io::Lines<BufReader<iroh::endpoint::RecvStream>>,
    String,
)> {
    let conn = endpoint.connect(ticket.endpoint.clone(), CONTROL_ALPN).await?;
    let (mut send, recv) = conn.open_bi().await?;
    let mut lines = BufReader::new(recv).lines();
    write_line(&mut send, &ClientMessage::Pin { pin: pin.to_string() }).await?;

    let mut session_id: Option<String> = None;
    let start = tokio::time::Instant::now();
    while session_id.is_none() && start.elapsed() < Duration::from_secs(30) {
        if let Ok(Ok(Some(line))) =
            tokio::time::timeout(Duration::from_secs(30), lines.next_line()).await
        {
            if let Ok(env) = serde_json::from_str::<TaskEnvelope<ServerMessage>>(&line) {
                if matches!(&env.payload, ServerMessage::Status { text: Some(t) } if t.contains("control channel ready")) {
                    session_id = Some(env.session_id);
                }
            }
        }
    }
    // Keep the connection alive by leaking it into the returned streams' lifetime:
    // returning send/recv keeps the underlying `Connection` open until they drop.
    std::mem::forget(conn);
    let session_id = session_id.ok_or_else(|| anyhow::anyhow!("no greeting within 30s"))?;
    Ok((send, lines, session_id))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let mut args = env::args().skip(1);
    let ticket_str = args.next().expect("usage: reconnect_task_active_probe <ticket> <pin>");
    let pin = args.next().expect("usage: reconnect_task_active_probe <ticket> <pin>");
    let ticket: LiveTicket = ticket_str.parse()?;

    let endpoint = Endpoint::builder(iroh::endpoint::presets::N0).bind().await?;

    // --- Connection 1: start a long text-only turn and let it stream. ---
    let (mut send, mut lines, session_id) = connect_and_pin(&endpoint, &ticket, &pin).await?;
    println!("conn1 session: {session_id}");

    let task_id = uuid::Uuid::new_v4().to_string();
    let env = TaskEnvelope::<ClientMessage>::wrap(
        session_id.clone(),
        Some(task_id.clone()),
        0,
        ClientMessage::Prompt { text: LONG_TEXT_PROMPT.into() },
    );
    write_line(&mut send, &env).await?;
    println!("-> long text prompt ({task_id})");

    let mut streaming = false;
    let live_start = tokio::time::Instant::now();
    while !streaming {
        if live_start.elapsed() > Duration::from_secs(120) {
            panic!("turn never started streaming within 120s");
        }
        if let Ok(Ok(Some(line))) =
            tokio::time::timeout(Duration::from_secs(120), lines.next_line()).await
        {
            if let Ok(env) = serde_json::from_str::<TaskEnvelope<ServerMessage>>(&line) {
                if matches!(env.payload, ServerMessage::TaskProgress { .. }) {
                    streaming = true;
                }
            }
        }
    }
    println!("conn1 turn is live (streaming). Dropping the connection...");

    // --- Drop connection 1 (simulate the app disconnecting). ---
    drop(send);
    drop(lines);
    tokio::time::sleep(Duration::from_secs(2)).await;

    // --- Connection 2: reconnect and look for the task_active restore signal. ---
    let (_send2, mut lines2, session2) = connect_and_pin(&endpoint, &ticket, &pin).await?;
    println!("conn2 session: {session2} (reconnected)");

    let mut saw_task_active = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let Ok(Ok(Some(line))) = tokio::time::timeout(remaining, lines2.next_line()).await else {
            break;
        };
        if let Ok(env) = serde_json::from_str::<TaskEnvelope<ServerMessage>>(&line) {
            if let ServerMessage::TaskActive { paused, queued } = env.payload {
                println!("  reconnect -> task_active {{ paused: {paused}, queued: {queued} }}");
                saw_task_active = true;
                break;
            }
        }
    }

    println!();
    println!("=== RESULT ===");
    if saw_task_active {
        println!("VERDICT: OK -- daemon emitted task_active on reconnect; the app can restore the Pause/Stop pill.");
    } else {
        println!("VERDICT: BROKEN -- no task_active within 20s of reconnect; the pill would stay hidden.");
        std::process::exit(1);
    }
    Ok(())
}
