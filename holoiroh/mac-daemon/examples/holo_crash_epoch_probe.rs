//! Manual, run-by-hand LIVE witness for the crash-restart stale-context guard
//! (`client_epoch`/`turn_epoch_is_stale`, see `holo_bridge::control`'s docs). Unlike
//! `epoch_mismatch_probe` (pure logic, no daemon), this drives a REAL turn against a running
//! daemon and expects an EXTERNAL actor (the orchestrating shell) to kill the actual `holo
//! serve` child process mid-turn -- reproducing the user-reported "stopped unexpectedly
//! (signal: 15 (SIGTERM))... restarted successfully... struggled to correct itself and failed"
//! sequence. This probe just observes and reports what the daemon does in response: does it
//! report an honest error instead of silently misfiring a doomed same-context redirect.
//!
//! Prompt is planning-only (never clicks/types/drags) so the probe never drives real desktop
//! actions -- same guard `live_task_control_probe` documents.
//!
//! Run with `cargo run --example holo_crash_epoch_probe -- <ticket> <pin>`, then externally
//! SIGTERM the `holo serve` child pid a few seconds after "phase: turn is live" prints.

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
    fifty distinct hypothetical uses for a brick. Pause briefly (a few seconds of thought) \
    between each one. Do not act on anything. Stop after the fiftieth.";

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
    let ticket_str = args.next().expect("usage: holo_crash_epoch_probe <ticket> <pin>");
    let pin = args.next().expect("usage: holo_crash_epoch_probe <ticket> <pin>");
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
    println!("PHASE: turn is live (progress observed) -- kill the holo-serve child pid now");

    // Watch for up to 3 minutes for whatever the daemon does in response to the external
    // crash: either the honest "backend restarted mid-task" Error this fix adds, or (if the
    // guard were absent/broken) a confusing second failure from a doomed same-context
    // redirect, or nothing at all (a genuine hang -- the exact "struggled ... and failed"
    // shape this fix targets).
    let outcome = wire
        .wait_for(Duration::from_secs(180), |env| {
            matches!(&env.payload, ServerMessage::Error { .. })
                || matches!(&env.payload, ServerMessage::TaskDone { .. })
        })
        .await;

    match outcome {
        Some(env) => println!("OUTCOME: daemon reached a terminal state -- {:?}", env.payload),
        None => println!(
            "OUTCOME: no Error/TaskDone within 180s of the crash -- turn appears to have hung \
             (this is the failure mode the fix targets; check daemon log for what happened)"
        ),
    }

    Ok(())
}
