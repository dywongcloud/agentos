//! Manual, run-by-hand LIVE witness for the crash-restart stale-context guard on the MANUAL
//! redirect path (`handle_redirect`'s `active_is_stale` branch, see `holo_bridge::control`'s
//! `client_epoch` docs). Companion to `holo_crash_epoch_probe` (which targets the
//! stall-watchdog's own 45s-window nudge path) -- this one targets `Redirect` directly, which
//! fires immediately on the client's own signal rather than waiting on a stall timer, making the
//! crash-then-redirect race deterministic to drive from an orchestrating shell instead of
//! depending on real backend timing.
//!
//! Sequence: prompt -> wait for live progress -> print `READY_FOR_KILL` (the external shell
//! kills the real `holo serve` child pid here) -> sleep a fixed window for the daemon's
//! health-check to detect + respawn -> send `Redirect` over the SAME still-open connection ->
//! observe what comes back. Expected (fix working): an `Error` mentioning the backend
//! restarting mid-task, followed by the redirect's own fresh turn running normally -- NOT a
//! silent hang and NOT a second unexplained failure from a doomed same-context retry.
//!
//! Run with `cargo run --example holo_crash_redirect_probe -- <ticket> <pin>`.

use std::env;
use std::time::Duration;

use holoiroh_daemon::control_channel::{
    write_line, ClientMessage, ServerMessage, TaskEnvelope, CONTROL_ALPN,
};
use iroh::Endpoint;
use iroh_live::ticket::LiveTicket;
use tokio::io::{AsyncBufReadExt, BufReader};

const PLANNING_ONLY: &str = "Do not click, type, drag, or invoke any tool -- this is a \
    planning-only exercise. In words only, think step by step and describe, one at a time, \
    fifty distinct hypothetical uses for a rubber band. Pause briefly between each one. Do not \
    act on anything. Stop after the fiftieth.";
const REDIRECT_TEXT: &str = "Do not click, type, drag, or invoke any tool -- planning only. \
    Forget the previous list; instead, in words only, name five colors, then stop.";

struct Wire<R> {
    lines: tokio::io::Lines<R>,
}

impl<R: tokio::io::AsyncBufRead + Unpin> Wire<R> {
    async fn next(&mut self, timeout: Duration) -> Option<TaskEnvelope<ServerMessage>> {
        loop {
            match tokio::time::timeout(timeout, self.lines.next_line()).await {
                Err(_) => return None,
                Ok(Ok(None)) => {
                    println!("!! stream closed by daemon");
                    return None;
                }
                Ok(Err(err)) => {
                    println!("!! read error: {err}");
                    return None;
                }
                Ok(Ok(Some(line))) => {
                    if line.trim().is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<TaskEnvelope<ServerMessage>>(&line) {
                        Ok(env) => {
                            println!(
                                "<- [{}] {:?} task_id={:?}",
                                env.message_type, env.payload, env.task_id
                            );
                            return Some(env);
                        }
                        Err(_) => println!("<- (unenveloped) {line}"),
                    }
                }
            }
        }
    }

    async fn wait_for(
        &mut self,
        deadline: Duration,
        mut pred: impl FnMut(&TaskEnvelope<ServerMessage>) -> bool,
    ) -> Option<TaskEnvelope<ServerMessage>> {
        let start = tokio::time::Instant::now();
        while start.elapsed() < deadline {
            let remaining = deadline.saturating_sub(start.elapsed());
            let Some(env) = self.next(remaining.min(Duration::from_secs(120))).await else {
                continue;
            };
            if pred(&env) {
                return Some(env);
            }
        }
        None
    }
}

fn is_progress(env: &TaskEnvelope<ServerMessage>) -> bool {
    matches!(env.payload, ServerMessage::TaskProgress { .. })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let mut args = env::args().skip(1);
    let ticket_str = args.next().expect("usage: holo_crash_redirect_probe <ticket> <pin>");
    let pin = args.next().expect("usage: holo_crash_redirect_probe <ticket> <pin>");
    let ticket: LiveTicket = ticket_str.parse()?;

    let endpoint = Endpoint::builder(iroh::endpoint::presets::N0).bind().await?;
    let conn = endpoint.connect(ticket.endpoint.clone(), CONTROL_ALPN).await?;
    println!("connected: remote={}", conn.remote_id().fmt_short());

    let (mut send, recv) = conn.open_bi().await?;
    let mut wire = Wire { lines: BufReader::new(recv).lines() };

    write_line(&mut send, &ClientMessage::Pin { pin }).await?;

    let greeting = wire
        .wait_for(Duration::from_secs(30), |env| {
            matches!(&env.payload, ServerMessage::Status { text: Some(t) } if t.contains("control channel ready"))
        })
        .await
        .expect("no greeting within 30s");
    let session_id = greeting.session_id.clone();
    println!("session established: {session_id}");
    let mut seq: u64 = 0;
    let mut next_seq = move || {
        let n = seq;
        seq += 1;
        n
    };

    let task_id = uuid::Uuid::new_v4().to_string();
    let env = TaskEnvelope::<ClientMessage>::wrap(
        session_id.clone(),
        Some(task_id.clone()),
        next_seq(),
        ClientMessage::Prompt { text: PLANNING_ONLY.into() },
    );
    write_line(&mut send, &env).await?;
    println!("-> prompt ({task_id})");

    wire.wait_for(Duration::from_secs(120), is_progress)
        .await
        .expect("no task_progress within 120s -- turn never started streaming");
    println!("READY_FOR_KILL");

    // Fixed window for the external shell to SIGTERM the real holo-serve child and for the
    // daemon's health-check to detect + respawn it (worst-case ~6s port-rebind TIME_WAIT per
    // `process.rs`'s spawn_inner doc, plus health-check tick latency) BEFORE this probe sends
    // its redirect -- so the redirect lands squarely inside the stale-epoch window the guard
    // exists to catch, deterministically, without depending on the turn's own stall timing.
    tokio::time::sleep(Duration::from_secs(12)).await;

    let redirect_id = uuid::Uuid::new_v4().to_string();
    let env = TaskEnvelope::<ClientMessage>::wrap(
        session_id.clone(),
        Some(redirect_id.clone()),
        next_seq(),
        ClientMessage::Redirect { text: REDIRECT_TEXT.into() },
    );
    write_line(&mut send, &env).await?;
    println!("-> redirect ({redirect_id}) sent after the crash window");

    // Watch everything that comes back for a while: expect an Ack for the redirect, an Error
    // about the backend restarting mid-task (the ORIGINAL turn, failed cleanly), then the
    // redirect's own fresh turn's progress/completion.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match wire.next(remaining.min(Duration::from_secs(90))).await {
            Some(env) if matches!(env.payload, ServerMessage::TaskDone { .. }) => {
                println!("OBSERVED terminal: {:?}", env.payload);
            }
            Some(_) => {}
            None => break,
        }
    }

    println!("DONE watching -- see log above for the full sequence");
    Ok(())
}
