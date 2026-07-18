//! holoiroh-daemon: Mac-side daemon.
//!
//! Startup sequence, in order, and why the order matters:
//!
//! 1. **Holo auth token check** (`auth::check_holo_token`). Must run before anything else --
//!    `holo_bridge` shells out to `holo` directly, and the whole point of the broadcast is to
//!    bridge to it. Proceeding without a token means `holo serve` would spawn and fail
//!    confusingly (or hang) rather than the daemon giving a clear "you forgot a step" message
//!    up front. Exits non-zero immediately on failure, before touching the network,
//!    permissions, or any subprocess.
//! 2. **macOS permission preflight** (`permissions::preflight`). Must run before the broadcast
//!    (`Live::publish`) starts -- publishing an empty/black `ScreenCaptureKit` feed because
//!    Screen Recording wasn't granted, or standing up a bridge that can't actually drive the
//!    Mac because Accessibility wasn't granted, is exactly the "broken/black stream" state
//!    this check exists to prevent. Refuses to start the broadcast if either is missing.
//! 3. **Only then**: bring up the `iroh-live` [`Live`] session, register an empty
//!    [`LocalBroadcast`], mount the bidirectional control channel ([`control_channel`]) on
//!    the same `iroh` `Endpoint`/`Router`, and -- best-effort -- start the `holo serve` bridge
//!    ([`holo_bridge`]) that the control channel forwards prompts to, plus the ongoing
//!    health-check loop ([`holo_bridge::health`]) that restarts `holo serve` on crash without
//!    ever touching the `Live` session or the control channel (see that module's doc for why
//!    this is a structural guarantee, not just a behavioral one).
//!
//! (See holoiroh/README.md for the full architecture, and `holo_bridge`'s module doc for the
//! bridge's source-grounded protocol details.)
//!
//! Capture (screen/audio) is not wired up yet -- the broadcast published here is empty (zero
//! tracks), even once permissions are confirmed granted (the preflight only proves the daemon
//! *could* capture, not that capture is implemented). Other mac-daemon modules build on the
//! `Live` + `LocalBroadcast` handles constructed here.

mod auth;
mod control_channel;
mod holo_bridge;
mod permissions;

use std::sync::Arc;

use iroh::protocol::Router;
use iroh_live::{Live, media::publish::LocalBroadcast, ticket::LiveTicket};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use control_channel::ControlChannel;
use holo_bridge::HoloBridge;

/// Name the daemon's broadcast is published under. A future iteration may
/// make this configurable (per-Mac identity, multiple concurrent
/// broadcasts); a single well-known name is sufficient for one daemon
/// publishing one stream today.
const BROADCAST_NAME: &str = "holoiroh";

/// `holo` CLI executable used to spawn `holo serve` (see
/// `holo_bridge::process`). Overridable so a dev machine can point at a
/// non-`PATH` binary without editing source.
fn holo_bin() -> String {
    std::env::var("HOLOIROH_HOLO_BIN").unwrap_or_else(|_| "holo".to_string())
}

/// Local port `holo serve` listens on. See `holo_bridge::process`.
fn holo_serve_port() -> u16 {
    std::env::var("HOLOIROH_HOLO_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8765)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    tracing::info!("holoiroh-daemon starting");

    // --- 1. Holo auth token check -- exit non-zero immediately if missing. ---
    match auth::check_holo_token() {
        Ok(token) => {
            info!(path = %token.path().display(), "Holo auth token found");
        }
        Err(err) => {
            eprintln!("[holoiroh-daemon] {err}");
            eprintln!("[holoiroh-daemon] {}", err.remediation());
            std::process::exit(1);
        }
    }

    // --- 2. macOS permission preflight -- refuse to start the broadcast if either Screen
    // Recording or Accessibility is missing, rather than producing a broken/black stream or a
    // bridge that can't actually drive the Mac. ---
    let preflight = permissions::preflight();
    if !preflight.is_ok() {
        preflight.report();
        eprintln!(
            "[holoiroh-daemon] Refusing to start broadcast: {} of 2 required macOS permission(s) missing.",
            preflight.missing.len()
        );
        std::process::exit(1);
    }
    info!("macOS permission preflight passed (Screen Recording + Accessibility granted)");

    // --- 3. iroh endpoint (reads IROH_SECRET if set, otherwise generates a
    // fresh key each run). Built *without* `.with_router()` because we own
    // the `Router` ourselves below, so both `Live`'s protocols (MoQ/gossip)
    // and the control channel's ALPN can be mounted on one shared
    // `Endpoint`/`Router` -- see `Live::register_protocols`'s own doc:
    // "If you already have a router ... skip [`with_router`] and call
    // `Live::register_protocols` on your own `RouterBuilder` instead." ---
    let live = Live::from_env().await?.spawn();
    info!(id = %live.endpoint().id(), "endpoint ready");

    // --- best-effort holo_bridge startup. A missing/unhealthy `holo`
    // binary must not prevent the daemon from publishing its broadcast or
    // accepting control-channel connections (which still work for
    // ack/status/error even without a bridge, e.g. surfacing "holo serve
    // unavailable" as a status message) -- so this is logged, not
    // propagated with `?`. ---
    let (bridge_events_tx, _bridge_events_rx) = mpsc::unbounded_channel();
    let bridge = match HoloBridge::start(holo_bin(), holo_serve_port(), bridge_events_tx).await {
        Ok(bridge) => {
            info!(pid = ?bridge.holo_serve_pid().await, "holo_bridge started");
            Some(Arc::new(bridge))
        }
        Err(err) => {
            warn!(error = %err, "holo_bridge failed to start -- control channel will run without it");
            None
        }
    };

    // --- build the shared Router: Live's own protocols (MoQ, gossip if
    // enabled) plus the control channel's ALPN, all on `live.endpoint()`.
    // See control_channel.rs's module doc for why this is "a second
    // logical stream on the same iroh QUIC connection" in iroh's
    // connection-per-ALPN model. ---
    let router_builder = Router::builder(live.endpoint().clone());
    let router_builder = live.register_protocols(router_builder);
    let router_builder = match bridge.clone() {
        Some(bridge) => {
            let control = ControlChannel::new(bridge);
            control.register_protocols(router_builder)
        }
        None => {
            info!("control channel not mounted: no holo_bridge available");
            router_builder
        }
    };
    let router = router_builder.spawn();

    // Empty broadcast: no video/audio source attached yet. `LocalBroadcast`
    // is immediately consumable/publishable with zero tracks -- callers
    // (this binary's future capture code) attach sources via
    // `broadcast.video()` / `broadcast.audio()` once capture lands.
    let broadcast = LocalBroadcast::new();

    // --- publish and print the shareable ticket ---
    live.publish(BROADCAST_NAME, &broadcast).await?;
    let ticket = LiveTicket::new(live.endpoint().addr(), BROADCAST_NAME);
    println!("{ticket}");
    info!(name = %BROADCAST_NAME, "publishing");

    // --- health-check loop: periodically verifies `holo serve` is still alive, restarts it
    // on crash without tearing down `live`/`router` above (this loop never touches either --
    // see `holo_bridge::health` module doc), and reports a DaemonStatus control event over
    // the same `bridge.control` sink the control channel already drains every time it does.
    // Only runs when `holo_bridge` actually started; nothing to supervise otherwise. ---
    let health_shutdown = CancellationToken::new();
    let health_check_task = bridge.clone().map(|bridge| {
        tokio::spawn(holo_bridge::run_health_check_loop(
            bridge,
            health_shutdown.clone(),
        ))
    });

    // --- wait for shutdown ---
    tokio::signal::ctrl_c().await?;
    tracing::info!("shutdown signal received");

    health_shutdown.cancel();
    if let Some(task) = health_check_task {
        let _ = task.await;
    }

    router.shutdown().await?;
    if let Some(bridge) = bridge {
        if let Ok(bridge) = Arc::try_unwrap(bridge) {
            if let Err(err) = bridge.shutdown().await {
                warn!(error = %err, "holo_bridge shutdown error");
            }
        }
    }
    live.shutdown().await;
    Ok(())
}
