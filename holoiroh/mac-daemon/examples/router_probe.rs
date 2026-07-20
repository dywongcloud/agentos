//! Witnesses `crate::router`'s classifier against real, representative prompts (no live
//! backend needed -- pure function), then optionally exercises a REAL model-switch respawn
//! against a live `holo serve` if `HOLOIROH_ROUTER_PROBE_LIVE=1` is set (uses paid H credits;
//! opt-in only).
//!
//! Run: `cargo run --example router_probe` (classifier only), or
//! `HOLOIROH_ROUTER_PROBE_LIVE=1 cargo run --example router_probe` (+ one real respawn).

use anyhow::{Context, Result, bail};
use holoiroh_daemon::holo_bridge::HoloServeProcess;
use holoiroh_daemon::router::{Tier, classify, should_switch};

const PROBE_A2A_PORT: u16 = 18792;
const PROBE_RUNTIME_PORT: &str = "18903";

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    // --- Classifier witness: every case must land on the expected tier. ---
    let cases: &[(&str, Tier)] = &[
        ("open safari", Tier::Simple),
        ("pause spotify", Tier::Simple),
        ("volume up", Tier::Simple),
        ("take a screenshot", Tier::Simple),
        (
            "Open Mail, find the latest email from my landlord, then open Notes and \
             summarize it, then open Calendar and schedule a reminder for the due date, \
             and finally send a reply in Mail confirming I saw it.",
            Tier::Complex,
        ),
        (
            "1. Open Discord and check for unread messages\n2. Open Slack and check for unread messages\n3. Summarize both in a new Note",
            Tier::Complex,
        ),
        (
            "Search Chrome for flights to NYC next weekend, compare prices across three sites, and if any is under $200 book it",
            Tier::Complex,
        ),
        ("what's the weather", Tier::Simple),
        ("close this window", Tier::Simple),
    ];

    let mut failures = Vec::new();
    for (prompt, expected) in cases {
        let got = classify(prompt);
        let mark = if got == *expected { "OK" } else { "FAIL" };
        println!("[{mark}] classify({prompt:?}) = {got:?} (expected {expected:?})");
        if got != *expected {
            failures.push(*prompt);
        }
    }

    // --- Hysteresis witness: a single borderline complex-adjacent prompt right after a
    // simple streak should NOT flip a Simple-active daemon (avoids respawn thrash). ---
    let borderline = "open mail and then check calendar";
    let borderline_score_tier = classify(borderline);
    let switch_from_simple = should_switch(Tier::Simple, borderline);
    println!(
        "[INFO] borderline prompt {borderline:?}: classify={borderline_score_tier:?} should_switch(Simple, ..)={switch_from_simple:?}"
    );

    let decisive_complex = "1. open mail\n2. open calendar\n3. open notes and summarize both, then reply to the most recent email";
    let switch_decisive = should_switch(Tier::Simple, decisive_complex);
    println!(
        "[{}] should_switch(Simple, decisive complex prompt) = {switch_decisive:?} (expected Some(Complex))",
        if switch_decisive == Some(Tier::Complex) { "OK" } else { "FAIL" }
    );
    if switch_decisive != Some(Tier::Complex) {
        failures.push("hysteresis-decisive-upgrade");
    }

    let stay_complex = should_switch(Tier::Complex, "open calendar");
    println!(
        "[INFO] should_switch(Complex, 'open calendar') = {stay_complex:?}"
    );

    if !failures.is_empty() {
        bail!("router classifier probe FAILED: {failures:?}");
    }
    println!("ROUTER CLASSIFIER: ALL CASES WITNESSED OK");

    if std::env::var("HOLOIROH_ROUTER_PROBE_LIVE").as_deref() != Ok("1") {
        println!(
            "(skipping live respawn witness -- set HOLOIROH_ROUTER_PROBE_LIVE=1 to exercise \
             a real holo serve --model switch; this spends real H credits)"
        );
        return Ok(());
    }

    live_respawn_witness()
}

/// Real end-to-end witness that BOTH model ids actually spawn a healthy `holo serve` --
/// the piece that cannot be confirmed by the classifier alone.
fn live_respawn_witness() -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        dotenvy::dotenv().ok();
        // Private runtime port (set per-iteration below, offset from PROBE_RUNTIME_PORT) so
        // this probe never touches a live daemon's runtime.
        let holo_bin = std::env::var("HOLOIROH_HOLO_BIN").unwrap_or_else(|_| {
            let installed = std::env::var("HOME").map(|h| format!("{h}/.holo/bin/holo"));
            match installed {
                Ok(p) if std::path::Path::new(&p).exists() => p,
                _ => "holo".to_string(),
            }
        });

        // Distinct ports per spawn (not just distinct runtime ports): a bare re-spawn on the
        // SAME A2A port right after `shutdown()` hit a real macOS TIME_WAIT window here that
        // outlived the daemon's own retry budget (8 attempts x 1.2s backoff) -- witnessed
        // live, not assumed. `holo serve`'s own within-daemon restart path never hits this
        // because it holds the port for the daemon's whole lifetime; only a probe spawning
        // twice in quick succession needs the workaround, so it lives here, not in process.rs.
        for (i, model) in [Tier::Simple.model_id(), Tier::Complex.model_id()].into_iter().enumerate() {
            let a2a_port = PROBE_A2A_PORT + i as u16;
            let runtime_port = (PROBE_RUNTIME_PORT.parse::<u16>().expect("valid port literal") + i as u16).to_string();
            unsafe { std::env::set_var("HOLOIROH_AGENT_RUNTIME_PORT", &runtime_port) };
            println!("spawning holo serve --model {model} on port {a2a_port} (runtime {runtime_port}) ...");
            let process = HoloServeProcess::spawn(&holo_bin, a2a_port, None, Some(model))
                .await
                .with_context(|| format!("holo serve failed to start with --model {model}"))?;
            let client = process.client();
            client
                .probe_agent_card()
                .await
                .with_context(|| format!("agent card probe failed for model {model}"))?;
            println!("[OK] {model}: holo serve healthy, agent card verified");
            process.shutdown().await.ok();
        }
        println!("ROUTER LIVE RESPAWN WITNESS: BOTH MODELS SPAWNED OK");
        Ok(())
    })
}
