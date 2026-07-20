//! Real end-to-end witness that `HoloControlBridge::run_prompt`'s environment-context
//! injection (see `crate::env_context`'s module doc) actually reaches the model: sends a
//! prompt through the REAL control bridge, against a REAL `holo serve` (local no-cloud mode --
//! no desktop actions possible, matching the daemon's own architecture), and asks the model to
//! literally state back which terminal app it now believes the user has -- a real behavioral
//! proof, not just a retrieval-layer unit check.
//!
//! Run: `cargo run --example env_context_injection_probe` (requires the real daemon's
//! `~/.holoiroh/context/` corpus to already be seeded -- run `env_context_seed` first).

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use holoiroh_daemon::holo_bridge::{ControlEvent, ControlMessage, HoloBridge};
use tokio::sync::mpsc;

const PROBE_A2A_PORT: u16 = 18794;
const PROBE_RUNTIME_PORT: &str = "18907";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("info,holoiroh_daemon=debug").init();
    dotenvy::dotenv().ok();
    unsafe { std::env::set_var("HOLOIROH_AGENT_RUNTIME_PORT", PROBE_RUNTIME_PORT) };

    let holo_bin = std::env::var("HOLOIROH_HOLO_BIN").unwrap_or_else(|_| {
        let installed = std::env::var("HOME").map(|h| format!("{h}/.holo/bin/holo"));
        match installed {
            Ok(p) if std::path::Path::new(&p).exists() => p,
            _ => "holo".to_string(),
        }
    });

    // Real local llama-server would be ideal (fully offline), but this daemon's actual
    // production path for a hosted turn is the primary H backend -- use that here (no
    // base_url override = primary/hosted, matching a real phone-originated turn exactly,
    // and it's a plain planning-only prompt so no real desktop action fires regardless).
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let bridge = Arc::new(
        HoloBridge::start(
            holo_bin,
            PROBE_A2A_PORT,
            None,
            None,
            std::time::Duration::from_secs(1800),
            events_tx,
        )
        .await
        .context("HoloBridge failed to start")?,
    );
    bridge.control.attach_bridge(Arc::downgrade(&bridge));

    println!("sending a prompt through the REAL control bridge (env-context injection should fire)...");
    bridge
        .handle_message(ControlMessage::Prompt {
            request_id: "env-context-injection-probe".to_string(),
            text: "Do not click, type, or invoke any tool. Just answer in words: based on any \
                   context you have been given about this user's environment, what terminal \
                   application do they actually use, and what should you do differently when \
                   asked to go to Claude Code?"
                .to_string(),
            context_id: None,
        })
        .await;

    let mut answer = String::new();
    while let Ok(event) = events_rx.try_recv() {
        println!("event: {event:?}");
        if let ControlEvent::Answer { text, .. } = event {
            answer = text;
        }
    }

    let lower = answer.to_lowercase();
    let mentions_ghostty = lower.contains("ghostty");
    println!("\n[{}] answer mentions Ghostty: {mentions_ghostty}", if mentions_ghostty { "OK" } else { "FAIL" });
    println!("answer: {answer}");

    if let Ok(owned) = Arc::try_unwrap(bridge) {
        owned.shutdown().await.ok();
    }

    if !mentions_ghostty {
        bail!(
            "ENV CONTEXT INJECTION NOT WITNESSED: the model's answer did not reference Ghostty \
             even when directly asked about the user's terminal -- injection may not be \
             reaching the model (check ~/.holoiroh/context/ is seeded, and control.rs's \
             augmented_text wiring)"
        );
    }
    println!("\nENV CONTEXT INJECTION: WITNESSED LIVE (the model correctly named Ghostty when asked)");
    Ok(())
}
