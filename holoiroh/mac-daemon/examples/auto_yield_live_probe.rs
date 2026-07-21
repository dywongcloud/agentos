//! Live end-to-end witness for cooperative auto-yield (the agent steps aside
//! while the user is active, resumes when they go idle). It dials the running
//! daemon, starts a LONG text-only turn, then drives the daemon's
//! `HOLOIROH_AUTO_YIELD_FORCE_IDLE_FILE` seam: writing "0" (user active) must
//! produce `task_active{paused:true}` (auto-pause), and writing a large idle
//! value (user idle) must produce `task_active{paused:false}` (auto-resume).
//!
//! Only the idle VALUE is injected -- the physical-vs-synthetic classifier is
//! witnessed separately (a synthetic event carries a nonzero source pid), and
//! cannot be exercised here because a synthetic event can never look physical.
//! Text-only prompt (no desktop actions). Requires a running daemon + holo serve
//! launched with `HOLOIROH_AUTO_YIELD_FORCE_IDLE_FILE=<file>`; NOT a headless CI
//! probe.
//!   cargo run --example auto_yield_live_probe -- <ticket> <pin> <force_idle_file>

use std::env;
use std::time::Duration;

use holoiroh_daemon::control_channel::{
    write_line, ClientMessage, ServerMessage, TaskEnvelope, CONTROL_ALPN,
};
use iroh::Endpoint;
use iroh_live::ticket::LiveTicket;
use tokio::io::{AsyncBufReadExt, BufReader};

const LONG_TEXT_PROMPT: &str = "Do not click, type, drag, open any app, or invoke ANY tool -- \
    writing only, never act on the desktop. Write an extremely detailed, long essay (1500+ words) \
    about the history of typewriters, slowly, section by section.";

fn set_idle(path: &str, secs: f64) {
    std::fs::write(path, format!("{secs}\n")).expect("write force-idle file");
}

/// Wait up to `budget` for a `task_active` frame with the given `paused` value.
async fn await_task_active(
    lines: &mut tokio::io::Lines<BufReader<iroh::endpoint::RecvStream>>,
    want_paused: bool,
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
                ServerMessage::TaskActive { paused, .. } if paused == want_paused => {
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
    let ticket_str = args.next().expect("usage: auto_yield_live_probe <ticket> <pin> <force_idle_file>");
    let pin = args.next().expect("usage: <pin>");
    let force_file = args.next().expect("usage: <force_idle_file>");
    let ticket: LiveTicket = ticket_str.parse()?;

    // Start with the user "idle" so the turn is allowed to begin.
    set_idle(&force_file, 60.0);

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

    // Start the long text-only turn and wait until it is streaming.
    let task_id = uuid::Uuid::new_v4().to_string();
    write_line(
        &mut send,
        &TaskEnvelope::<ClientMessage>::wrap(session_id.clone(), Some(task_id), 0, ClientMessage::Prompt { text: LONG_TEXT_PROMPT.into() }),
    )
    .await?;
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
    assert!(streaming, "turn never started streaming");
    println!("turn is live. Simulating the USER becoming active (idle=0)...");

    // User becomes active -> expect auto-pause.
    set_idle(&force_file, 0.0);
    let paused = await_task_active(&mut lines, true, Duration::from_secs(10)).await;

    // User goes idle again -> expect auto-resume.
    println!("Simulating the user going idle again (idle=60)...");
    set_idle(&force_file, 60.0);
    let resumed = await_task_active(&mut lines, false, Duration::from_secs(15)).await;

    // Leave the file in the idle state so the daemon doesn't keep yielding.
    set_idle(&force_file, 60.0);

    println!();
    println!("=== RESULT ===");
    if paused && resumed {
        println!("VERDICT: OK -- auto-yield paused when the user went active and resumed when they went idle.");
    } else {
        println!("VERDICT: BROKEN -- auto_pause_seen={paused} auto_resume_seen={resumed}");
        std::process::exit(1);
    }
    Ok(())
}
