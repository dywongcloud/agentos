//! End-to-end witness of the LIVE rate-limit failover: a turn fails on the primary backend,
//! the control bridge suppresses the failure, `HoloBridge::activate_fallback` terminates the
//! healthy `holo serve` and respawns it on the tinfoil backend, and the SAME turn retries to
//! a real answer -- the exact user-facing "agent backend error keeps happening" scenario.
//!
//! The hosted failure is simulated hermetically: the PRIMARY inference target points at a
//! dead loopback port (`127.0.0.1:9`), so every hosted-turn inference call fails and
//! `holo serve` emits its literal generic `"agent backend error"` terminal -- byte-identical
//! to how the real H-backend 429s surface (serve.py wraps ALL agent-API HTTPErrors in that
//! one string; witnessed in the installed source). No API keys are tampered with.
//!
//! Run: `cargo run --example fallback_switch_probe`
//!
//! Isolation from a live daemon: A2A port 18791, runtime port 18902 -- probe-private.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use holoiroh_daemon::holo_bridge::{ControlEvent, ControlMessage, HoloBridge, InferenceTarget};
use holoiroh_daemon::tinfoil_proxy::{DEFAULT_UPSTREAM, TinfoilProxy};
use tokio::sync::mpsc;

const PROBE_A2A_PORT: u16 = 18791;
const PROBE_RUNTIME_PORT: &str = "18902";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("info,holoiroh_daemon=debug")
        .init();

    dotenvy::dotenv().ok();
    let key = std::env::var("TINFOIL_API_KEY")
        .context("TINFOIL_API_KEY missing -- put it in mac-daemon/.env")?;
    unsafe { std::env::set_var("HOLOIROH_AGENT_RUNTIME_PORT", PROBE_RUNTIME_PORT) };

    let proxy = TinfoilProxy::spawn(DEFAULT_UPSTREAM, key.trim()).await?;
    let primary = InferenceTarget {
        // Dead on arrival by construction: port 9 (discard) is never listening on loopback.
        base_url: "http://127.0.0.1:9/v1".to_string(),
        model: None,
        label: "dead-primary (probe stand-in for a rate-limited hosted backend)".to_string(),
    };
    let fallback = InferenceTarget {
        base_url: format!("{}/v1", proxy.local_url()),
        model: Some("kimi-k2-6".to_string()),
        label: "kimi-k2-6 (tinfoil)".to_string(),
    };

    let holo_bin = std::env::var("HOLOIROH_HOLO_BIN").unwrap_or_else(|_| {
        let installed = std::env::var("HOME").map(|h| format!("{h}/.holo/bin/holo"));
        match installed {
            Ok(p) if std::path::Path::new(&p).exists() => p,
            _ => "holo".to_string(),
        }
    });

    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let bridge = Arc::new(
        HoloBridge::start(
            holo_bin,
            PROBE_A2A_PORT,
            Some(primary),
            Some(fallback),
            Duration::from_secs(1800),
            events_tx,
        )
        .await
        .context("HoloBridge failed to start on the dead primary")?,
    );
    bridge.control.attach_bridge(Arc::downgrade(&bridge));
    println!("bridge up on dead primary; sending the turn that must fail over...");

    // handle_message awaits the WHOLE turn -- including the suppressed failure, the
    // backend switch, and the retry -- before returning.
    bridge
        .handle_message(ControlMessage::Prompt {
            request_id: "failover-probe".to_string(),
            text: "Do not click or type anything. Immediately finish and answer with the single word: ping".to_string(),
            context_id: None,
        })
        .await;

    let mut saw_switch_note = false;
    let mut answer: Option<String> = None;
    let mut failure: Option<String> = None;
    while let Ok(event) = events_rx.try_recv() {
        println!("event: {event:?}");
        match event {
            ControlEvent::Progress { text: Some(t), .. } if t.contains("fallback model") => {
                saw_switch_note = true;
            }
            ControlEvent::Answer { text, .. } => answer = Some(text),
            ControlEvent::Error { message, .. } => failure = Some(message),
            _ => {}
        }
    }

    println!(
        "on_fallback={} switch_note={} answer={:?} failure={:?}",
        bridge.is_on_fallback(),
        saw_switch_note,
        answer,
        failure
    );
    // Sole strong ref (control holds only a Weak backref), so unwrap succeeds
    // and the graceful shutdown path runs; the child's kill_on_drop is the net.
    if let Ok(owned) = Arc::try_unwrap(bridge) {
        owned.shutdown().await.ok();
    }

    if !saw_switch_note || !bridge_answered_ping(answer.as_deref()) {
        bail!("FAILOVER CHAIN NOT WITNESSED (switch_note={saw_switch_note}, answer={answer:?}, failure={failure:?})");
    }
    println!("LIVE FAILOVER CHAIN: WITNESSED OK (failed turn -> tinfoil switch -> same turn answered)");
    Ok(())
}

fn bridge_answered_ping(answer: Option<&str>) -> bool {
    answer
        .map(|a| a.to_ascii_lowercase().contains("ping"))
        .unwrap_or(false)
}
