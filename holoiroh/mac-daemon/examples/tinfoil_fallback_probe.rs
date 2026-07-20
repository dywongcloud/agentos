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
            |update| println!("update: {update:?}"),
        )
        .await;

    let outcome = match &result {
        Ok(ctx) => format!("OK (context {ctx})"),
        Err(err) => format!("ERR {err:#}"),
    };
    println!("turn outcome: {outcome}");

    process.shutdown().await.ok();
    if result.is_err() {
        bail!("tinfoil fallback chain FAILED");
    }
    println!("TINFOIL FALLBACK CHAIN: WITNESSED OK");
    Ok(())
}
