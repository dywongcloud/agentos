//! Manual, run-by-hand probe: the LIVE witness for the sensitive-app privacy gate (PRD §9
//! class-5) -- dials the running daemon over real iroh, starts a planning-only turn, then
//! brings **System Settings** (bundle `com.apple.systempreferences`, in the seeded
//! `system_security_settings` category, default `always_ask`) frontmost on this Mac so the
//! per-turn watchdog's real frontmost-app poll trips the gate:
//!
//! 1. expect the turn to PAUSE and a `sensitive_access_consent` `input_request` to arrive
//!    with options `["Allow once", "Stop task"]`;
//! 2. answer `Allow once` via a real `input_response`;
//! 3. expect the consent-granted status and the turn to resume streaming;
//! 4. cleanup: stop the turn, return focus by quitting System Settings.
//!
//! Run with `cargo run --example consent_probe -- <ticket> <pin>` on the daemon's own Mac
//! (the probe must be able to change that Mac's frontmost app).
//!
//! Set `CONSENT_PROBE_EXPECT=hard_block` (with the daemon's
//! `~/.holoiroh/sensitive_categories.toml` edited to `setting = "hard_block"` for
//! `system_security_settings` and the daemon restarted) to witness the HardBlock arm
//! instead: the turn is stopped outright with the "hard-blocked" status, no consent ask.

use std::env;
use std::time::Duration;

use holoiroh_daemon::control_channel::{
    write_line, ClientMessage, ServerMessage, TaskEnvelope, CONTROL_ALPN,
};
use iroh::Endpoint;
use iroh_live::ticket::LiveTicket;
use tokio::io::{AsyncBufReadExt, BufReader};

const PLANNING_ONLY: &str = "Do not click, type, drag, or invoke any tool -- this is a \
    planning-only exercise. In words only, enumerate twenty-five factors to weigh when \
    choosing a programming language for a new project, one at a time. Do not act on \
    anything. Stop after the twenty-fifth.";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let mut args = env::args().skip(1);
    let ticket_str = args.next().expect("usage: consent_probe <ticket> <pin>");
    let pin = args.next().expect("usage: consent_probe <ticket> <pin>");
    let ticket: LiveTicket = ticket_str.parse()?;

    let endpoint = Endpoint::builder(iroh::endpoint::presets::N0).bind().await?;
    let conn = endpoint.connect(ticket.endpoint.clone(), CONTROL_ALPN).await?;
    let (mut send, recv) = conn.open_bi().await?;
    let mut lines = BufReader::new(recv).lines();

    write_line(&mut send, &ClientMessage::Pin { pin }).await?;

    // Greeting -> session id.
    let mut session_id: Option<String> = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while session_id.is_none() && tokio::time::Instant::now() < deadline {
        if let Ok(Ok(Some(line))) =
            tokio::time::timeout(Duration::from_secs(30), lines.next_line()).await
        {
            if let Ok(env) = serde_json::from_str::<TaskEnvelope<ServerMessage>>(&line) {
                if matches!(&env.payload, ServerMessage::Status { text: Some(t) } if t.contains("control channel ready"))
                {
                    session_id = Some(env.session_id);
                }
            }
        }
    }
    let session_id = session_id.expect("no greeting within 30s");
    println!("session established: {session_id}");
    let mut seq: u64 = 0;

    // Start a planning-only turn.
    let task_id = uuid::Uuid::new_v4().to_string();
    let env = TaskEnvelope::<ClientMessage>::wrap(
        session_id.clone(),
        Some(task_id.clone()),
        seq,
        ClientMessage::Prompt { text: PLANNING_ONLY.into() },
    );
    seq += 1;
    write_line(&mut send, &env).await?;
    println!("-> prompt ({task_id})");

    let expect_hard_block =
        std::env::var("CONSENT_PROBE_EXPECT").as_deref() == Ok("hard_block");
    // Which consent option to answer with ("Allow once" default; set
    // CONSENT_PROBE_ANSWER="Stop task" to witness the deny arm: the parked
    // turn is discarded and the daemon reports the task stopped).
    let answer =
        std::env::var("CONSENT_PROBE_ANSWER").unwrap_or_else(|_| "Allow once".to_string());

    // Wait for real streaming progress, then trip the gate.
    let mut saw_progress = false;
    let mut consent: Option<(String, Vec<String>)> = None;
    let mut settings_opened = false;
    let overall = tokio::time::Instant::now();
    while consent.is_none() {
        if overall.elapsed() > Duration::from_secs(420) {
            panic!("no gate outcome within 420s (progress seen: {saw_progress})");
        }
        let Ok(Ok(Some(line))) =
            tokio::time::timeout(Duration::from_secs(120), lines.next_line()).await
        else {
            continue;
        };
        let Ok(env) = serde_json::from_str::<TaskEnvelope<ServerMessage>>(&line) else {
            continue;
        };
        match &env.payload {
            ServerMessage::TaskProgress { .. } => {
                if !saw_progress {
                    println!("turn is live (progress observed)");
                }
                saw_progress = true;
                if !settings_opened {
                    // Bring a sensitive-category app frontmost ON THE DAEMON'S MAC --
                    // the watchdog's real lsappinfo poll does the rest.
                    let status = std::process::Command::new("open")
                        .args(["-a", "System Settings"])
                        .status()?;
                    println!("opened System Settings (status {status}) -- waiting for the gate");
                    settings_opened = true;
                }
            }
            ServerMessage::Status { text: Some(t) } if expect_hard_block && t.contains("hard-blocked") => {
                println!("<- {t}");
                println!("PASS(hard_block): watchdog stopped the turn outright for the hard-blocked category");
                let env = TaskEnvelope::<ClientMessage>::wrap(
                    session_id.clone(),
                    Some(uuid::Uuid::new_v4().to_string()),
                    seq,
                    ClientMessage::Stop,
                );
                write_line(&mut send, &env).await?;
                let _ = std::process::Command::new("osascript")
                    .args(["-e", "tell application \"System Settings\" to quit"])
                    .status();
                println!();
                println!("consent_probe: OK -- hard_block arm witnessed live.");
                return Ok(());
            }
            ServerMessage::InputRequest {
                request_id,
                kind,
                context,
                response_options,
                ..
            } => {
                println!("<- input_request kind={kind:?} context={context:?} options={response_options:?}");
                assert!(
                    matches!(kind, holoiroh_daemon::control_channel::InputRequestKind::SensitiveAccessConsent),
                    "unexpected input_request kind: {kind:?}"
                );
                assert!(
                    response_options.iter().any(|o| o == "Allow once"),
                    "expected an 'Allow once' option, got {response_options:?}"
                );
                consent = Some((request_id.clone(), response_options.clone()));
            }
            other => println!("<- {other:?}"),
        }
    }
    let (consent_id, _) = consent.unwrap();
    println!("PASS(gate): sensitive-app watchdog paused the turn and asked for consent");

    // Answer with the configured option.
    let env = TaskEnvelope::<ClientMessage>::wrap(
        session_id.clone(),
        Some(uuid::Uuid::new_v4().to_string()),
        seq,
        ClientMessage::InputResponse {
            request_id: consent_id,
            selected_option: answer.clone(),
        },
    );
    seq += 1;
    write_line(&mut send, &env).await?;
    println!("-> input_response: {answer}");

    if answer != "Allow once" {
        // Deny arm: expect the consent-denied status; the turn stays stopped.
        let deny_deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        loop {
            if tokio::time::Instant::now() > deny_deadline {
                panic!("no consent-denied status within 60s");
            }
            let Ok(Ok(Some(line))) =
                tokio::time::timeout(Duration::from_secs(60), lines.next_line()).await
            else {
                continue;
            };
            let Ok(env) = serde_json::from_str::<TaskEnvelope<ServerMessage>>(&line) else {
                continue;
            };
            if let ServerMessage::Status { text: Some(t) } = &env.payload {
                if t.contains("consent denied") {
                    println!("<- {t}");
                    break;
                }
            }
        }
        let _ = std::process::Command::new("osascript")
            .args(["-e", "tell application \"System Settings\" to quit"])
            .status();
        println!();
        println!("consent_probe: OK -- deny arm witnessed live: consent denied stopped the task.");
        return Ok(());
    }

    // Expect consent-granted status then fresh progress (the resumed turn).
    let mut saw_granted = false;
    let mut saw_resumed_progress = false;
    let resume_deadline = tokio::time::Instant::now() + Duration::from_secs(420);
    while !(saw_granted && saw_resumed_progress) {
        if tokio::time::Instant::now() > resume_deadline {
            panic!("consent-allow did not resume (granted={saw_granted}, progress={saw_resumed_progress})");
        }
        let Ok(Ok(Some(line))) =
            tokio::time::timeout(Duration::from_secs(120), lines.next_line()).await
        else {
            continue;
        };
        let Ok(env) = serde_json::from_str::<TaskEnvelope<ServerMessage>>(&line) else {
            continue;
        };
        match &env.payload {
            ServerMessage::Status { text: Some(t) } if t.contains("consent granted") => {
                println!("<- {t}");
                saw_granted = true;
            }
            ServerMessage::TaskProgress { .. } if saw_granted => {
                saw_resumed_progress = true;
            }
            _ => {}
        }
    }
    println!("PASS(allow): consent granted resumed the turn (fresh progress observed)");

    // Cleanup: stop the resumed turn; close System Settings.
    let env = TaskEnvelope::<ClientMessage>::wrap(
        session_id.clone(),
        Some(uuid::Uuid::new_v4().to_string()),
        seq,
        ClientMessage::Stop,
    );
    write_line(&mut send, &env).await?;
    let _ = std::process::Command::new("osascript")
        .args(["-e", "tell application \"System Settings\" to quit"])
        .status();

    println!();
    println!("consent_probe: OK -- privacy gate witnessed live: pause on sensitive app, consent ask, allow-once resume.");
    Ok(())
}
