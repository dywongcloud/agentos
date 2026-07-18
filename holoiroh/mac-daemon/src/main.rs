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
//! Screen capture (via [`capture::setup_screen_video`], macOS
//! ScreenCaptureKit, `--display <index>` selectable) is wired up as the
//! broadcast's video source before publish. System/mic audio capture is not
//! wired up yet.

mod allowlist;
mod capture;
mod control_channel;
mod holo_bridge;

use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use iroh::protocol::Router;
use iroh_live::{
    Live,
    media::{codec::VideoCodec, format::VideoPreset, publish::LocalBroadcast},
    ticket::LiveTicket,
};
use tokio::sync::mpsc;
use tracing::{info, warn};

use allowlist::generate_default_pin;
use control_channel::ControlChannel;
use holo_bridge::HoloBridge;

/// Name the daemon's broadcast is published under. A future iteration may
/// make this configurable (per-Mac identity, multiple concurrent
/// broadcasts); a single well-known name is sufficient for one daemon
/// publishing one stream today.
const BROADCAST_NAME: &str = "holoiroh";

/// CLI arguments for `holoiroh-daemon`.
#[derive(Parser, Debug)]
#[command(name = "holoiroh-daemon", about = "Mac-side holoiroh P2P daemon")]
struct Cli {
    /// Which display to capture when multiple are connected, by index into
    /// the list `iroh_live::media::capture::ScreenCapturer::list_all()`
    /// returns (same ordering `capture::list_displays()` exposes). Omit to
    /// use the primary display.
    #[arg(long)]
    display: Option<usize>,

    /// Disable the first-connection PIN gate (see `allowlist.rs` and
    /// `holoiroh/PAIRING.md`'s "Auth beyond ticket possession" section) --
    /// every connection is accepted immediately with no PIN and no
    /// allowlist enforcement, matching this daemon's pre-auth behavior.
    /// Intended for local dev/testing against a same-machine or trusted-LAN
    /// peer only; a real deployment should leave PIN auth enabled (the
    /// default).
    #[arg(long)]
    no_pin_auth: bool,
}

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

    let cli = Cli::parse();

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

    // --- first-connection PIN, generated fresh every daemon run (never
    // persisted -- only the resulting allowlist entry is). `--no-pin-auth`
    // disables this entirely for local dev/testing (see Cli::no_pin_auth's
    // doc); real usage leaves it on by default so a leaked ticket alone
    // does not grant control, per README.md's "Security model" section and
    // `holoiroh/PAIRING.md`. ---
    let pin = if cli.no_pin_auth {
        None
    } else {
        Some(generate_default_pin())
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
            let control = match pin.clone() {
                Some(pin) => ControlChannel::with_auth(bridge, pin),
                None => ControlChannel::new(bridge),
            };
            control.register_protocols(router_builder)
        }
        None => {
            info!("control channel not mounted: no holo_bridge available");
            router_builder
        }
    };
    let router = router_builder.spawn();

    // Broadcast with the ScreenCaptureKit video source attached -- no audio
    // source yet. `capture::setup_screen_video` resolves `--display` (or the
    // primary display when omitted) and calls `broadcast.video().set_source(..)`
    // on our behalf.
    let broadcast = LocalBroadcast::new();
    capture::setup_screen_video(
        &broadcast,
        cli.display,
        VideoCodec::H264,
        &[VideoPreset::P720],
    )?;

    // --- publish and print the shareable ticket ---
    live.publish(BROADCAST_NAME, &broadcast).await?;
    let ticket = LiveTicket::new(live.endpoint().addr(), BROADCAST_NAME);
    println!("{ticket}");
    // PIN is printed as plain text alongside the ticket -- this is the
    // real, reachable slice of the pairing UX `PAIRING.md` designs (a QR
    // rendering of `ticket` above it is documented there but not yet
    // implemented in this binary; see that file's "Implementation status"
    // table for exactly what's real vs designed-only).
    if let Some(pin) = &pin {
        println!("pairing PIN (first connection only): {pin}");
    } else {
        println!("PIN auth disabled (--no-pin-auth): any device with the ticket can connect");
    }
    info!(name = %BROADCAST_NAME, "publishing");

    // --- wait for shutdown ---
    //
    // `ctrlc::set_handler` (not `tokio::signal::ctrl_c()`) so both SIGINT *and* SIGTERM trigger
    // graceful cleanup: `ctrl_c()` alone only ever fires on SIGINT, which would silently skip
    // the explicit shutdown sequence below (dropping/closing the iroh `Live` session +
    // `LocalBroadcast`, terminating the tracked `holo serve` child) on a plain `kill` or
    // launchd/Docker stop, both of which send SIGTERM by default. `ctrlc`'s handler runs on its
    // own dedicated OS thread and is not `async`, so it only flips a channel to wake the async
    // task below rather than doing any cleanup itself.
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let shutdown_tx = std::sync::Mutex::new(Some(shutdown_tx));
    ctrlc::set_handler(move || {
        if let Some(tx) = shutdown_tx.lock().unwrap().take() {
            let _ = tx.send(());
        }
    })
    .context("failed to register SIGINT/SIGTERM handler")?;
    let _ = shutdown_rx.await;

    info!("shutdown signal received, cleaning up");
    router.shutdown().await?;
    // Explicitly drop the broadcast before `live.shutdown()` -- `LocalBroadcast` owns its own
    // media pipelines/track handles and releases them in its own `Drop` impl (see vendored
    // `iroh-live`'s `moq-media/src/publish.rs`); dropping it here (rather than letting it fall
    // out of scope implicitly at the end of `main`) makes that release happen deterministically
    // as part of this shutdown sequence, before the underlying `Live` session it was published
    // through goes away.
    drop(broadcast);
    if let Some(bridge) = bridge {
        match Arc::try_unwrap(bridge) {
            Ok(bridge) => {
                if let Err(err) = bridge.shutdown().await {
                    warn!(error = %err, "holo_bridge shutdown error");
                }
            }
            Err(bridge) => {
                // Another Arc clone is still alive (e.g. a control-channel connection handler
                // still running its accept loop) -- we can't consume `HoloBridge::shutdown(self)`
                // through a shared reference. Falling out of scope here still runs
                // `HoloServeProcess`'s `Drop` safety net (best-effort SIGTERM + kill_on_drop)
                // once every clone is gone, so the child is not left orphaned; it just won't get
                // the graceful awaited SIGTERM-then-wait path `shutdown()` provides.
                warn!(
                    refs = Arc::strong_count(&bridge),
                    "holo_bridge still has other Arc references at shutdown; falling back to Drop-based cleanup instead of graceful shutdown()"
                );
            }
        }
    }
    live.shutdown().await;
    Ok(())
}
