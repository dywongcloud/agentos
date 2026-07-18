//! holoiroh-daemon: Mac-side daemon.
//!
//! This is the P2P publish entrypoint: it brings up an `iroh-live` [`Live`]
//! session, registers an empty [`LocalBroadcast`], publishes it under a
//! well-known name, and prints the resulting shareable ticket to stdout so
//! it can be pasted/scanned into the iOS client (see holoiroh/README.md for
//! the full architecture). Alongside the broadcast, it mounts the
//! bidirectional control channel ([`control_channel`]) on the same `iroh`
//! `Endpoint`/`Router`, and -- best-effort -- starts the `holo serve`
//! bridge ([`holo_bridge`]) that the control channel forwards prompts to.
//!
//! Capture (screen/audio) is not wired up yet -- this binary proves out the
//! P2P publish/ticket half of the pipeline plus the full control-channel
//! path. Other mac-daemon modules build on the `Live` + `LocalBroadcast`
//! handles constructed here.

mod control_channel;
mod holo_bridge;

use std::sync::Arc;

use iroh::protocol::Router;
use iroh_live::{Live, media::publish::LocalBroadcast, ticket::LiveTicket};
use tokio::sync::mpsc;
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

    // --- iroh endpoint (reads IROH_SECRET if set, otherwise generates a
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
            info!(pid = ?bridge.holo_serve_pid(), "holo_bridge started");
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

    // --- wait for shutdown ---
    tokio::signal::ctrl_c().await?;
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
