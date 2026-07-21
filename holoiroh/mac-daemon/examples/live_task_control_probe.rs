//! Manual, run-by-hand probe: the LIVE end-to-end witness for mid-task control -- dials a
//! running holoiroh-daemon over real iroh (same transport/envelope discipline as
//! `control_probe`) and drives a real agent turn through each control verb:
//!
//! - **A. stop mid-turn**: prompt -> wait for real streaming progress -> `stop` -> assert the
//!   stop's ACK arrives while the turn is still alive (the read loop is no longer parked
//!   inside the turn -- the exact regression this whole change fixes) and the turn terminates
//!   as canceled.
//! - **B. pause / resume**: prompt -> progress -> `pause` (expect the paused status) ->
//!   `resume` (expect the resuming status and fresh progress). Same-`contextId` continuity is
//!   witnessed daemon-side (grep its log for the two "turn contextId resolved" lines).
//! - **C. redirect**: while the resumed turn streams -> `redirect` with a different
//!   instruction (expect the redirecting status and the new turn's events) -> final cleanup
//!   `stop`.
//!
//! Every prompt is planning-only ("do not click/type/use any tool") so the probe never
//! drives real desktop actions -- the same hard-learned guard `tinfoil_fallback_probe`
//! documents.
//!
//! Run with `cargo run --example live_task_control_probe -- <ticket> <pin>`.

use std::env;
use std::time::Duration;

use holoiroh_daemon::control_channel::{
    write_line, ClientMessage, ServerMessage, TaskEnvelope, CONTROL_ALPN,
};
use iroh::Endpoint;
use iroh_live::ticket::LiveTicket;
use tokio::io::{AsyncBufReadExt, BufReader};

const PLANNING_ONLY_A: &str = "Do not click, type, drag, or invoke any tool -- this is a \
    planning-only exercise. In words only, think step by step and describe, one at a time, \
    forty distinct hypothetical uses for a paperclip. Do not act on anything. Stop after the \
    fortieth.";
const PLANNING_ONLY_B: &str = "Do not click, type, drag, or invoke any tool -- this is a \
    planning-only exercise. In words only, list and briefly explain thirty considerations for \
    planning a week-long hiking trip. Do not act on anything. Stop after the thirtieth.";
const REDIRECT_TEXT: &str = "Do not click, type, drag, or invoke any tool -- planning only. \
    Forget the previous list; instead, in words only, name ten breeds of dog and one fact \
    about each, then stop.";

struct Wire<R> {
    lines: tokio::io::Lines<R>,
}

impl<R: tokio::io::AsyncBufRead + Unpin> Wire<R> {
    /// Next decoded envelope within `timeout`, or None on timeout.
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

    /// Reads until `pred` matches (returning that envelope) or `deadline` elapses (None).
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

fn status_contains(env: &TaskEnvelope<ServerMessage>, needle: &str) -> bool {
    matches!(&env.payload, ServerMessage::Status { text: Some(t) } if t.contains(needle))
}

fn is_progress(env: &TaskEnvelope<ServerMessage>) -> bool {
    matches!(env.payload, ServerMessage::TaskProgress { .. })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let mut args = env::args().skip(1);
    let ticket_str = args.next().expect("usage: live_task_control_probe <ticket> <pin>");
    let pin = args.next().expect("usage: live_task_control_probe <ticket> <pin>");
    let ticket: LiveTicket = ticket_str.parse()?;

    let endpoint = Endpoint::builder(iroh::endpoint::presets::N0).bind().await?;
    let conn = endpoint.connect(ticket.endpoint.clone(), CONTROL_ALPN).await?;
    println!("connected: remote={}", conn.remote_id().fmt_short());

    let (mut send, recv) = conn.open_bi().await?;
    let mut wire = Wire { lines: BufReader::new(recv).lines() };

    // PIN handshake (bare) -- harmless if this device is already allowlisted:
    // the daemon acks a redundant Pin instead of erroring.
    write_line(&mut send, &ClientMessage::Pin { pin }).await?;

    // Greeting -> session_id. (An already-allowlisted device gets the
    // greeting immediately; the redundant Pin then earns a later ack we
    // simply ignore in the event flow.)
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

    // ---------- Phase A: stop mid-turn ----------
    let task_a = uuid::Uuid::new_v4().to_string();
    let env = TaskEnvelope::<ClientMessage>::wrap(
        session_id.clone(),
        Some(task_a.clone()),
        next_seq(),
        ClientMessage::Prompt { text: PLANNING_ONLY_A.into() },
    );
    write_line(&mut send, &env).await?;
    println!("-> prompt A ({task_a})");

    wire.wait_for(Duration::from_secs(300), is_progress)
        .await
        .expect("phase A: no task_progress within 300s -- turn never started streaming");
    println!("phase A: turn is live (progress observed)");

    let stop_id = uuid::Uuid::new_v4().to_string();
    let env = TaskEnvelope::<ClientMessage>::wrap(
        session_id.clone(),
        Some(stop_id.clone()),
        next_seq(),
        ClientMessage::Stop,
    );
    write_line(&mut send, &env).await?;
    let stop_sent = tokio::time::Instant::now();
    println!("-> stop (mid-turn)");

    // The ACK arriving promptly is THE regression witness: with the old
    // inline-await read loop, no inbound line was even read until the turn
    // finished, so this ack could not arrive while the turn was alive.
    let ack = wire
        .wait_for(Duration::from_secs(20), |env| {
            matches!(env.payload, ServerMessage::Ack { .. }) && env.task_id.as_deref() == Some(stop_id.as_str())
        })
        .await
        .expect("phase A: stop was not acked within 20s of sending -- read loop still parked?");
    println!(
        "phase A PASS(ack): stop acked {}ms after send, mid-turn (task_id={:?})",
        stop_sent.elapsed().as_millis(),
        ack.task_id
    );

    wire.wait_for(Duration::from_secs(120), |env| {
        matches!(&env.payload, ServerMessage::TaskDone { status, .. } if status == "canceled")
    })
    .await
    .expect("phase A: no canceled task_done within 120s of stop");
    println!("phase A PASS: turn terminated as canceled after mid-stream stop");

    // ---------- Phase B: pause / resume ----------
    let task_b = uuid::Uuid::new_v4().to_string();
    let env = TaskEnvelope::<ClientMessage>::wrap(
        session_id.clone(),
        Some(task_b.clone()),
        next_seq(),
        ClientMessage::Prompt { text: PLANNING_ONLY_B.into() },
    );
    write_line(&mut send, &env).await?;
    println!("-> prompt B ({task_b})");

    wire.wait_for(Duration::from_secs(300), is_progress)
        .await
        .expect("phase B: no task_progress within 300s");
    println!("phase B: turn is live (progress observed)");

    let pause_id = uuid::Uuid::new_v4().to_string();
    let env = TaskEnvelope::<ClientMessage>::wrap(
        session_id.clone(),
        Some(pause_id),
        next_seq(),
        ClientMessage::Pause,
    );
    write_line(&mut send, &env).await?;
    println!("-> pause (mid-turn)");

    wire.wait_for(Duration::from_secs(60), |env| status_contains(env, "task paused"))
        .await
        .expect("phase B: no 'task paused' status within 60s");
    println!("phase B PASS(pause): daemon confirmed the pause");

    let resume_id = uuid::Uuid::new_v4().to_string();
    let env = TaskEnvelope::<ClientMessage>::wrap(
        session_id.clone(),
        Some(resume_id),
        next_seq(),
        ClientMessage::Resume,
    );
    write_line(&mut send, &env).await?;
    println!("-> resume");

    wire.wait_for(Duration::from_secs(30), |env| status_contains(env, "resuming"))
        .await
        .expect("phase B: no 'resuming' status within 30s");
    wire.wait_for(Duration::from_secs(300), is_progress)
        .await
        .expect("phase B: no post-resume task_progress within 300s");
    println!("phase B PASS: resumed turn is streaming (contextId continuity witnessed in the daemon log)");

    // ---------- Phase C: redirect ----------
    let redirect_id = uuid::Uuid::new_v4().to_string();
    let env = TaskEnvelope::<ClientMessage>::wrap(
        session_id.clone(),
        Some(redirect_id.clone()),
        next_seq(),
        ClientMessage::Redirect { text: REDIRECT_TEXT.into() },
    );
    write_line(&mut send, &env).await?;
    println!("-> redirect (mid-turn)");

    wire.wait_for(Duration::from_secs(60), |env| status_contains(env, "redirecting"))
        .await
        .expect("phase C: no 'redirecting' status within 60s");
    wire.wait_for(Duration::from_secs(300), |env| {
        is_progress(env) && env.task_id.as_deref() == Some(redirect_id.as_str())
    })
    .await
    .expect("phase C: no task_progress under the redirect's task_id within 300s");
    println!("phase C PASS: redirected instruction is streaming under its own task_id");

    // Cleanup: stop whatever is still running so the daemon idles clean.
    let env = TaskEnvelope::<ClientMessage>::wrap(
        session_id.clone(),
        Some(uuid::Uuid::new_v4().to_string()),
        next_seq(),
        ClientMessage::Stop,
    );
    write_line(&mut send, &env).await?;
    println!("-> cleanup stop");
    wire.wait_for(Duration::from_secs(60), |env| {
        matches!(&env.payload, ServerMessage::TaskDone { status, .. } if status == "canceled")
    })
    .await;

    println!();
    println!(
        "live_task_control_probe: OK -- stop/pause/resume/redirect all witnessed mid-turn \
         against the live daemon over real iroh."
    );
    Ok(())
}
