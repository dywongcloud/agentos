//! End-to-end witness for the tinfoil rate-limit fallback chain, WITHOUT touching a live
//! daemon: loopback auth proxy -> `holo serve --base-url <proxy>/v1 --model kimi-k2-6` ->
//! real A2A task -> answer streamed back, all against the REAL Tinfoil endpoint with the
//! real key from `mac-daemon/.env`.
//!
//! Run: `cargo run --example tinfoil_fallback_probe`
//!
//! Isolation from a concurrently-running daemon (never kill the operator's session):
//! - A2A port 18790 (daemon default differs), runtime port 18901 via
//!   `HOLOIROH_AGENT_RUNTIME_PORT` (daemon default 18899, `holo` CLI default 18795) --
//!   this probe spawns and reaps only its OWN `hai-agent-runtime`.

use anyhow::{Context, Result, bail};
use holoiroh_daemon::holo_bridge::HoloServeProcess;
use holoiroh_daemon::tinfoil_proxy::{DEFAULT_UPSTREAM, TinfoilProxy};

const PROBE_A2A_PORT: u16 = 18790;
const PROBE_RUNTIME_PORT: &str = "18901";
// Distinct port for the complex-prompt stage: the hai-agent-runtime process has its OWN
// local session-creation rate limit (witnessed live: a real 429 from
// 127.0.0.1:<runtime-port>/api/v2/sessions after the trivial-prompt stage's session, even
// after a 5s backoff) -- a fresh runtime (fresh port, fresh process) sidesteps that limiter
// entirely rather than guessing at its real cooldown window.
const PROBE_RUNTIME_PORT_2: &str = "18906";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("info,holoiroh_daemon=debug")
        .init();

    dotenvy::dotenv().ok();
    let key = std::env::var("TINFOIL_API_KEY")
        .context("TINFOIL_API_KEY missing -- put it in mac-daemon/.env")?;
    // Private runtime port BEFORE any spawn, so this probe cannot attach to (or reap) the
    // daemon's own runtime. set_var is safe here: single-threaded startup, before spawns.
    unsafe { std::env::set_var("HOLOIROH_AGENT_RUNTIME_PORT", PROBE_RUNTIME_PORT) };

    // 1. The auth-injecting loopback proxy.
    let proxy = TinfoilProxy::spawn(DEFAULT_UPSTREAM, key.trim()).await?;
    let base_url = format!("{}/v1", proxy.local_url());
    println!("proxy up: {base_url} -> {DEFAULT_UPSTREAM}");

    // 2. holo serve pointed at it, with the tinfoil vision model. Resolve the binary the
    // same way the daemon does (HOLOIROH_HOLO_BIN, else ~/.holo/bin/holo, else PATH).
    let holo_bin = std::env::var("HOLOIROH_HOLO_BIN").unwrap_or_else(|_| {
        let installed = std::env::var("HOME").map(|h| format!("{h}/.holo/bin/holo"));
        match installed {
            Ok(p) if std::path::Path::new(&p).exists() => p,
            _ => "holo".to_string(),
        }
    });
    let process = HoloServeProcess::spawn(&holo_bin, PROBE_A2A_PORT, Some(&base_url), Some("kimi-k2-6"))
        .await
        .context("holo serve failed to start against the tinfoil proxy")?;
    let client = process.client();
    client.probe_agent_card().await.context("agent card probe failed")?;
    println!("holo serve healthy on port {PROBE_A2A_PORT}, agent card OK");

    // 3. One real task straight through the whole chain.
    let result = client
        .send_and_stream(
            "Do not click or type anything. Immediately finish and answer with the single word: ping",
            None,
            |ids| println!("turn ids resolved: {ids:?}"),
            |update| println!("update: {update:?}"),
        )
        .await;

    let outcome = match &result {
        Ok(ctx) => format!("OK (context {ctx})"),
        Err(err) => format!("ERR {err:#}"),
    };
    println!("turn outcome: {outcome}");

    if result.is_err() {
        process.shutdown().await.ok();
        bail!("tinfoil fallback chain FAILED");
    }
    println!("TINFOIL FALLBACK CHAIN (trivial prompt): WITNESSED OK");
    process.shutdown().await.ok();

    // Fresh runtime port + fresh holo serve for the complex-prompt stage: the
    // hai-agent-runtime process has its own local session-creation rate limit, independent of
    // tinfoil's -- reusing the same runtime process for a second session hit a real 429 from
    // 127.0.0.1:<runtime-port>/api/v2/sessions (witnessed live, even after a 5s backoff). A
    // fresh runtime process sidesteps that limiter entirely instead of guessing its cooldown.
    unsafe { std::env::set_var("HOLOIROH_AGENT_RUNTIME_PORT", PROBE_RUNTIME_PORT_2) };
    let proxy2 = TinfoilProxy::spawn(DEFAULT_UPSTREAM, key.trim()).await?;
    let base_url2 = format!("{}/v1", proxy2.local_url());
    let process = HoloServeProcess::spawn(&holo_bin, PROBE_A2A_PORT, Some(&base_url2), Some("kimi-k2-6"))
        .await
        .context("holo serve failed to start (stage 2)")?;
    let client = process.client();
    client.probe_agent_card().await.context("agent card probe failed (stage 2)")?;
    println!("holo serve (stage 2) healthy on port {PROBE_A2A_PORT}, agent card OK");

    // 4. The kimi-tuning regression witness: a genuinely complex multi-step SCENARIO that,
    // pre-fix, exhausted the runtime's hardcoded 2048-completion-token budget on Kimi's
    // reasoning preamble alone -- finish_reason "length", zero answer content, ~30-100s
    // burned per attempt (measured directly against the raw endpoint before this fix). With
    // tinfoil_proxy.rs's apply_kimi_tuning (thinking:false, response_format translation,
    // 6000-token floor) now live in this same proxy, this exact shape of prompt should reach
    // a real Answer within the daemon's default timeouts.
    //
    // Explicitly forbids real actions ("do not click, type, or use any tool -- describe your
    // plan in words only, then stop"): an earlier version of this probe used a bare
    // instruction ("Open Mail, find...") with no such guard, and the real desktop-agent
    // genuinely executed it for 10+ minutes unattended against this machine's actual Mail.app
    // (clicking sidebar items, opening Settings, typing into the real search field) before
    // being caught and killed -- a real mistake, not a hypothetical one. This probe verifies
    // Kimi's reasoning/token-budget behavior on a complex prompt; it must never be the thing
    // that decides whether real desktop actions are safe to fire.
    let complex_start = std::time::Instant::now();
    let mut saw_answer = false;
    let mut saw_working = 0u32;
    let complex_result = client
        .send_and_stream(
            "Do not click, type, drag, or invoke any tool -- this is a planning-only exercise. \
             Describe in words only (no tool calls) how you would: open Mail, find the latest \
             email from a landlord, open Notes and summarize it, open Calendar and schedule a \
             reminder for the due date, then reply in Mail confirming you saw it. When your \
             description is complete, stop -- do not act on it.",
            None,
            |ids| println!("turn ids resolved (stage 2): {ids:?}"),
            |update| {
                match &update {
                    holoiroh_daemon::holo_bridge::a2a_client::TaskUpdate::Answer { text } => {
                        saw_answer = !text.trim().is_empty();
                        println!("answer ({} chars): {}", text.len(), &text[..text.len().min(200)]);
                    }
                    holoiroh_daemon::holo_bridge::a2a_client::TaskUpdate::Working { .. } => {
                        saw_working += 1;
                    }
                    _ => {}
                }
                println!("update: {update:?}");
            },
        )
        .await;
    let elapsed = complex_start.elapsed();
    println!(
        "complex-prompt turn: {:?} elapsed={elapsed:?} saw_working={saw_working} saw_answer={saw_answer}",
        complex_result.as_ref().map(|_| "OK").unwrap_or("ERR")
    );

    process.shutdown().await.ok();
    if complex_result.is_err() || !saw_answer {
        bail!(
            "KIMI TUNING REGRESSION: complex prompt did not produce a real answer \
             (result={complex_result:?}, saw_answer={saw_answer}) -- the pre-fix failure mode"
        );
    }
    println!("KIMI TUNING WITNESS: OK (complex multi-step prompt answered in {elapsed:?}, no truncation)");
    Ok(())
}
