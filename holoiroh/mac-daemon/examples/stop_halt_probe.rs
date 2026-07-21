//! Adversarial live witness for issue-1 ("I pressed Stop and the agent kept going"): dials the
//! running daemon over real iroh, starts a LONG text-only turn, sends Stop once it is streaming,
//! then COUNTS how many `task_progress` events still arrive in the window after the stop. A
//! working stop yields ~0 post-stop progress and a prompt `task_done`; a broken (graceful-only,
//! ignored-by-the-agent) stop keeps streaming progress for many seconds -- exactly the user's
//! report.
//!
//! Text-only prompt (explicitly forbids tools) so the probe drives NO real desktop actions --
//! same safety discipline as the other live probes. The "length" comes from asking for a long
//! written answer, which keeps the backend turn streaming long enough to interrupt.
//!
//! Run with `cargo run --example stop_halt_probe -- <ticket> <pin>`.

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
    extremely detailed, long essay (at least 1500 words) about the history and engineering of \
    suspension bridges, section by section, going slowly and thoroughly. Keep writing until you \
    have covered materials, cables, towers, decks, aerodynamics, and famous examples in depth.";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let mut args = env::args().skip(1);
    let ticket_str = args.next().expect("usage: stop_halt_probe <ticket> <pin>");
    let pin = args.next().expect("usage: stop_halt_probe <ticket> <pin>");
    let ticket: LiveTicket = ticket_str.parse()?;

    let endpoint = Endpoint::builder(iroh::endpoint::presets::N0).bind().await?;
    let conn = endpoint.connect(ticket.endpoint.clone(), CONTROL_ALPN).await?;
    let (mut send, recv) = conn.open_bi().await?;
    let mut lines = BufReader::new(recv).lines();

    write_line(&mut send, &ClientMessage::Pin { pin }).await?;

    // Greeting -> session id.
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
    let session_id = session_id.expect("no greeting within 30s");
    println!("session established: {session_id}");
    let mut seq: u64 = 0;

    // Start the long text-only turn.
    let task_id = uuid::Uuid::new_v4().to_string();
    let env = TaskEnvelope::<ClientMessage>::wrap(
        session_id.clone(),
        Some(task_id.clone()),
        seq,
        ClientMessage::Prompt { text: LONG_TEXT_PROMPT.into() },
    );
    seq += 1;
    write_line(&mut send, &env).await?;
    println!("-> long text prompt ({task_id})");

    // Wait for the first task_progress (turn is genuinely streaming).
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
    println!("turn is live (streaming). Letting it run 3s, then sending Stop...");
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Send Stop.
    let stop_id = uuid::Uuid::new_v4().to_string();
    let env = TaskEnvelope::<ClientMessage>::wrap(
        session_id.clone(),
        Some(stop_id),
        seq,
        ClientMessage::Stop,
    );
    write_line(&mut send, &env).await?;
    let stop_at = tokio::time::Instant::now();
    println!("-> STOP sent");

    // Two distinct counts:
    //  - `buffered_at_stop`: progress arriving BEFORE the canceled terminal (already in the SSE
    //    pipe when stop landed -- NOT the agent continuing).
    //  - `after_terminal`: progress arriving AFTER the canceled terminal -- this is the TRUE
    //    "the agent is still running" signal and must be 0 for a working stop.
    // Listen a full 12s past the terminal to give a still-running backend time to betray itself.
    let mut buffered_at_stop = 0u32;
    let mut after_terminal = 0u32;
    let mut terminal_at: Option<tokio::time::Instant> = None;
    let mut terminal_ms: Option<u128> = None;
    let overall_deadline = stop_at + Duration::from_secs(15);
    loop {
        let now = tokio::time::Instant::now();
        // Stop 12s after the terminal (or at the overall deadline).
        if let Some(t) = terminal_at {
            if now.duration_since(t) > Duration::from_secs(12) {
                break;
            }
        }
        if now >= overall_deadline && terminal_at.is_none() {
            break;
        }
        let remaining = overall_deadline.saturating_duration_since(now).max(Duration::from_millis(1));
        let Ok(Ok(Some(line))) = tokio::time::timeout(remaining, lines.next_line()).await else {
            break;
        };
        let Ok(env) = serde_json::from_str::<TaskEnvelope<ServerMessage>>(&line) else {
            continue;
        };
        match &env.payload {
            ServerMessage::TaskProgress { .. } => {
                if terminal_at.is_some() {
                    after_terminal += 1;
                    println!("  [+{}ms] AFTER-TERMINAL task_progress #{after_terminal} (agent still running!)", stop_at.elapsed().as_millis());
                } else {
                    buffered_at_stop += 1;
                }
            }
            ServerMessage::TaskDone { status, .. } if status == "canceled" && terminal_at.is_none() => {
                terminal_at = Some(tokio::time::Instant::now());
                terminal_ms = Some(stop_at.elapsed().as_millis());
                println!("  [+{}ms] canceled terminal", stop_at.elapsed().as_millis());
            }
            _ => {}
        }
    }

    println!();
    println!("=== RESULT ===");
    println!("progress buffered at stop (pre-terminal, expected): {buffered_at_stop}");
    println!("progress AFTER the canceled terminal (the bug signal): {after_terminal}");
    println!("canceled terminal seen: {} (at {terminal_ms:?}ms after stop)", terminal_at.is_some());
    if terminal_at.is_none() {
        println!("VERDICT: BROKEN -- no canceled terminal within 15s of stop");
        std::process::exit(1);
    } else if after_terminal > 0 {
        println!("VERDICT: BROKEN -- {after_terminal} progress events streamed AFTER the terminal; the agent kept running past the stop");
        std::process::exit(1);
    } else {
        println!("VERDICT: OK -- stop halted the backend (0 progress after the canceled terminal; {buffered_at_stop} pre-terminal buffered events are in-flight SSE, not continuation)");
    }
    Ok(())
}
