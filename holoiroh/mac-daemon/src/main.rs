//! holoiroh-daemon: Mac-side daemon.
//!
//! This is the P2P publish entrypoint. It performs the following steps:
//!
//! - brings up an `iroh-live` [`Live`] session
//! - registers an empty [`LocalBroadcast`]
//! - publishes the broadcast under a well-known name
//! - prints the resulting shareable ticket to stdout, so it can be pasted or scanned into the
//!   iOS client (see holoiroh/README.md for the full architecture)
//!
//! Alongside the broadcast, it mounts the bidirectional control channel ([`control_channel`])
//! on the same `iroh` `Endpoint`/`Router`. Best-effort, it also starts the `holo serve` bridge
//! ([`holo_bridge`]) that the control channel forwards prompts to.
//!
//! The daemon wires up screen capture (via [`capture::setup_screen_video`], macOS
//! ScreenCaptureKit, `--display <index>` selectable) as the broadcast's video source before
//! publish. System/mic audio capture is not wired up yet.

mod accessibility_tree;
mod action_executor;
mod agent_guidance;
mod agent_loop;
mod allowlist;
mod approval;
mod audit_log;
mod auth;
mod auto_yield;
mod capture;
mod clarify;
mod control_channel;
mod duration;
mod remote_input;
mod user_activity;
// NOTE: `executor` (the ComputerUseExecutor abstraction seam, PRD 7.3) is deliberately declared
// only in `lib.rs`, not here. It is an available seam consumed by `examples/executor_probe.rs`
// via the `holoiroh_daemon` lib crate; wiring the live daemon's control path to route through it
// (rather than calling `HoloBridge` directly, as `main.rs` does today) is a separate follow-on and
// is intentionally out of this pass's scope. Declaring `mod executor;` in the binary target too
// would compile the whole seam as dead code here (25 warnings), since nothing in `main.rs`
// references it yet -- so it lives in the lib target only until that wiring lands.
mod env_context;
mod execution_mode;
mod frontmost_app;
mod holo_bridge;
mod instance_guard;
mod limits;
mod local_llama_proxy;
mod local_model;
mod pairing_phrase;
mod permissions;
mod policy;
mod privacy;
mod process_awareness;
mod registry;
mod router;
mod sensitive_categories;
mod semantic_ax;
mod task_fsm;
mod task_state;
mod tinfoil_audio;
mod tinfoil_client;
mod tinfoil_documents;
mod tinfoil_models;
mod tinfoil_planner;
mod tinfoil_proxy;
mod tinfoil_vision;
mod tmux;

use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use iroh::EndpointAddr;
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
/// broadcasts). A single well-known name is sufficient for one daemon
/// publishing one stream today.
const BROADCAST_NAME: &str = "holoiroh";
const TINFOIL_INIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

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

    /// Selects whether autonomous Holo prompts are admitted.
    #[arg(long, value_enum, default_value_t)]
    execution_mode: execution_mode::ExecutionMode,

    /// Disable the first-connection PIN gate (see `allowlist.rs` and
    /// `holoiroh/PAIRING.md`'s "Auth beyond ticket possession" section). Every connection is
    /// then accepted immediately with no PIN and no allowlist enforcement, matching this
    /// daemon's pre-auth behavior. Intended only for local dev/testing against a same-machine
    /// or trusted-LAN peer. A real deployment should leave PIN auth enabled (the default).
    #[arg(long)]
    no_pin_auth: bool,

    /// Re-print the pairing ticket and verification phrase on a fixed interval while the daemon
    /// keeps running (e.g. `30m`, `2h`, `1h30m`). This way, a stale QR screenshot stops being
    /// the one the operator is reading off. See `holoiroh/mac-daemon/PAIRING.md`'s "Ticket
    /// rotation" section. NOTE: this re-prints the *current* ticket. A full
    /// fresh-keypair-per-tick identity rotation (which invalidates old tickets entirely)
    /// requires tearing down and rebuilding the iroh `Live` session mid-run. That is documented
    /// there as a separate, larger step. Rotation-on-restart already happens implicitly (fresh
    /// keypair per process start when `IROH_SECRET` is unset). The device allowlist also gives
    /// device-level rotation protection regardless.
    #[arg(long, value_parser = duration::parse_rotate_duration)]
    rotate_every: Option<std::time::Duration>,

    /// Check whether this machine can actually run the daemon. Print what it found. Then exit.
    ///
    /// This flag checks, against the real APIs, everything a pairing failure usually turns out
    /// to be -- rather than only describing it:
    ///
    /// - Accessibility and Screen Recording grants
    /// - which display the daemon would capture, and at what rate
    /// - whether the iroh endpoint binds with local-network discovery registered
    ///
    /// This flag deliberately stops short of publishing a broadcast, spawning `holo serve`, or
    /// opening the control channel. It is therefore safe to run while another daemon is already
    /// serving a phone. It cannot steal that daemon's single client slot, and it cannot fight
    /// that daemon for a port.
    #[arg(long)]
    preflight: bool,
}

/// Reports whether this machine can run the daemon, without becoming one.
async fn run_preflight(live: &Live, display_index: Option<usize>) -> anyhow::Result<()> {
    // Run the checks, THEN close, whatever the outcome. Closing only on the success path meant
    // every early return dropped the endpoint instead, and iroh logs "Endpoint dropped without
    // calling Endpoint::close. Aborting ungracefully." at ERROR -- so the one run where something
    // is genuinely wrong printed a scary unrelated-looking line right next to the real reason.
    // Witnessed with `--preflight --display 99`.
    let outcome = preflight_checks(live, display_index).await;
    live.endpoint().close().await;
    outcome
}

async fn preflight_checks(live: &Live, display_index: Option<usize>) -> anyhow::Result<()> {
    use iroh_live::media::capture::ScreenCapturer;
    use iroh_live::media::traits::VideoSource;

    println!("holoiroh preflight");
    println!("  endpoint id      {}", live.endpoint().id());
    println!("  bound sockets    {:?}", live.endpoint().bound_sockets());
    println!(
        "  local discovery  registered (mDNS), so a phone on this network can find this Mac \
         without going through a relay"
    );

    let accessibility = remote_input::is_permitted();
    println!(
        "  accessibility    {}",
        if accessibility {
            "granted -- remote clicks and typing will work"
        } else {
            "DENIED -- the phone can watch this Mac but not drive it. Grant it in System \
             Settings > Privacy & Security > Accessibility."
        }
    );

    let monitor = capture::resolve_display(display_index)?;
    println!("  display          {}", monitor.summary());

    let mut capturer = ScreenCapturer::with_monitor_config(&monitor, &capture::screen_config())?;
    capturer.start()?;
    let window = std::time::Duration::from_secs(2);
    let started = std::time::Instant::now();
    let mut frames = 0u32;
    while started.elapsed() < window {
        if matches!(capturer.pop_frame(), Ok(Some(_))) {
            frames += 1;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    capturer.stop()?;
    let observed = f64::from(frames) / window.as_secs_f64();
    println!(
        "  capture          {observed:.1} fps observed against {:.0} configured",
        capture::CAPTURE_FPS
    );

    anyhow::ensure!(
        frames > 0,
        "captured no frames at all. Screen Recording is almost certainly not granted to this \
         binary -- System Settings > Privacy & Security > Screen Recording."
    );
    anyhow::ensure!(
        accessibility,
        "Accessibility is not granted, so hands-on control would silently do nothing"
    );
    println!("\npreflight OK -- this machine can publish its screen and accept remote control.");
    Ok(())
}

/// `holo` CLI executable used to spawn `holo serve` (see
/// `holo_bridge::process`). Overridable via `HOLOIROH_HOLO_BIN` so a dev
/// machine can point at a non-`PATH` binary without editing source.
///
/// Falls back to `~/.holo/bin/holo` (the path `holo login`'s own installer
/// writes -- see `auth.rs`'s module doc) when bare `"holo"` is not resolvable
/// on `PATH`. Without this fallback, this function would always emit the literal string
/// `"holo"` and let `tokio::process::Command::spawn` fail with an opaque
/// `No such file or directory (os error 2)`. This was witnessed live: `holo` is
/// genuinely absent from a plain non-interactive shell's `PATH` (it's only on
/// the user's own interactive shell rc). This fallback is therefore not hypothetical.
fn holo_bin() -> String {
    if let Ok(v) = std::env::var("HOLOIROH_HOLO_BIN") {
        return v;
    }
    if which_on_path("holo").is_none() {
        if let Some(home) = std::env::var_os("HOME") {
            let fallback = std::path::Path::new(&home).join(".holo/bin/holo");
            if fallback.is_file() {
                return fallback.to_string_lossy().into_owned();
            }
        }
    }
    "holo".to_string()
}

/// Minimal `PATH`-search for `name`, mirroring what `Command::spawn` itself
/// does for a bare (non-slash-containing) program name. Used only to decide
/// whether [`holo_bin`]'s `~/.holo/bin/holo` fallback is needed -- not as a
/// general-purpose `which` replacement.
fn which_on_path(name: &str) -> Option<std::path::PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// Local port `holo serve` listens on. See `holo_bridge::process`.
fn holo_serve_port() -> u16 {
    std::env::var("HOLOIROH_HOLO_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8765)
}

/// Print the shareable ticket as a QR code, its raw text, and its verification phrase. This is
/// the exact block re-emitted both at startup and on each `--rotate-every` rotation tick, so
/// the two stay byte-identical. `context` is a short label (e.g. "pairing" / "rotated") shown
/// in the header line, so the operator can tell a rotation apart from the initial print.
fn print_pairing_block(ticket_str: &str, context: &str) {
    println!("--- {context} ticket ---");
    print_ticket_qr(ticket_str);
    println!("{ticket_str}");
    println!(
        "verification phrase (must match the iOS app): {}",
        pairing_phrase::pairing_phrase(ticket_str)
    );
}

/// Render `ticket` as a scannable QR code to stdout using unicode block
/// characters, per PAIRING.md's terminal-rendering design. Best-effort: on a
/// QR-construction failure (e.g. the ticket string somehow exceeding QR
/// capacity), this function logs the error and skips the QR, instead of aborting startup.
/// The raw ticket text printed alongside it is always the authoritative fallback.
fn print_ticket_qr(ticket: &str) {
    // EcLevel::L (lowest error correction) minimizes the QR version and thus the
    // module count: the ~230-byte ticket needs version 11 (61x61 modules) at
    // `QrCode::new`'s implicit EcLevel::M default, but only version 9-10
    // (53x53 / 57x57) at L. Low ECC is fine here -- the code is scanned straight
    // off a pristine screen, not a damaged printed label.
    match qrcode::QrCode::with_error_correction_level(ticket.as_bytes(), qrcode::EcLevel::L) {
        Ok(code) => {
            // Dense1x2 packs two vertically-adjacent modules into one character
            // cell (' ', '▄', '▀', '█'), so the code is HALF the terminal height
            // of the old one-row-per-module `render::<char>()` output and each
            // module is roughly square in a typical ~1:2 terminal font --
            // ~31 rows x ~61 cols instead of 69x69, small enough to fit on
            // screen unscrolled and far easier for a phone camera to lock onto.
            // quiet_zone(true) keeps the 4-module light border scanners require.
            let rendered = code
                .render::<qrcode::render::unicode::Dense1x2>()
                .quiet_zone(true)
                .build();
            println!("Scan this QR with the iOS app (or paste the ticket below):");
            println!("{rendered}");
        }
        Err(err) => {
            warn!(error = %err, "could not render ticket QR code; use the raw ticket text below");
        }
    }
}

/// Whether the daemon should run its own local `llama-server` (Aro Private mode) and point
/// `holo serve` at it. The alternative is leaving `holo serve` on the hosted Holo3 API (via
/// `HAI_API_KEY`).
///
/// Defaults to **off** (hosted API) as of this build. Starting the local `llama-server` means
/// loading a 21GB model, which can take minutes with no output before the daemon prints
/// anything. This was witnessed live as a silent, indistinguishable-from-hung startup on a
/// plain `holoiroh-daemon` invocation with no env vars set (the exact symptom reported: "just
/// hangs, no QR code shows up"). Set `HOLOIROH_LOCAL_MODEL=1` (or `true`/`yes`) to opt IN to
/// local inference (Project Aro PRD P0-11's no-cloud-path mode), once that tradeoff is wanted
/// again.
/// The daemon's iroh identity key, STABLE across restarts.
///
/// `IROH_SECRET` env wins when set (iroh-live's own convention, unchanged).
/// Otherwise, this function loads the key from `~/.holoiroh/iroh_secret` (hex, 0600, same
/// config dir as `allowlist.json`), or generates it there for the first time. Without this,
/// every daemon restart minted a fresh random identity (`SecretKey::generate` inside
/// `iroh_live::util::secret_key_from_env`). This changed the node id, and therefore the
/// pairing ticket. It silently invalidated every saved connection profile in the iOS app, and
/// forced a QR re-scan on every restart.
fn persistent_secret_key() -> anyhow::Result<iroh::SecretKey> {
    if std::env::var("IROH_SECRET").is_ok() {
        return Ok(iroh_live::util::secret_key_from_env()?);
    }
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME not set; cannot locate ~/.holoiroh/iroh_secret"))?;
    let dir = home.join(".holoiroh");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join("iroh_secret");
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return trimmed
                .parse::<iroh::SecretKey>()
                .with_context(|| format!("parsing persisted key at {}", path.display()));
        }
    }
    let key = iroh::SecretKey::generate();
    let hex = data_encoding::HEXLOWER.encode(&key.to_bytes());
    std::fs::write(&path, format!("{hex}\n"))
        .with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    info!(path = %path.display(), "generated and persisted a new iroh identity key");
    Ok(key)
}

fn local_model_enabled() -> bool {
    match std::env::var("HOLOIROH_LOCAL_MODEL") {
        Ok(v) => matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"),
        Err(_) => false,
    }
}

/// True if this machine has a working IPv6 default route, per `route -n get
/// -inet6 default`'s stdout (macOS-only, matching this crate's
/// `cfg(target_os = "macos")` scope elsewhere -- see `permissions.rs`).
/// Having a link-local (`fe80::`) address on an interface is NOT sufficient
/// -- that's present even on IPv4-only networks and cannot route to a real
/// peer -- so this checks for an actual default route, not interface
/// presence.
///
/// Deliberately checks **stdout content**, not exit status: `route`'s own
/// behavior on "no route" is to print "route: writing to routing socket:
/// not in table" to STDERR while still exiting **0** (verified live on this
/// exact machine -- a first attempt at this check trusted exit status alone
/// and would have silently always returned `true`, defeating the fix this
/// function exists for). On a real route, stdout carries structured
/// `destination:`/`gateway:`/etc. fields; on "not in table", stdout is
/// empty. Checking for a non-empty stdout is therefore the actual signal.
/// Any error running `route` itself (missing binary, permission issue) is
/// treated as "no v6 route" -- the conservative choice, since an IPv4-only
/// bind always works, while wrongly assuming v6 works risks the exact stall
/// this check exists to prevent. See the call site in `main()` (iroh
/// endpoint construction) for the full rationale.
fn has_ipv6_default_route() -> bool {
    std::process::Command::new("route")
        .args(["-n", "get", "-inet6", "default"])
        .stderr(std::process::Stdio::null())
        .output()
        .map(|output| !output.stdout.is_empty())
        .unwrap_or(false)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // An explicit fallback filter, because `fmt::init()` alone defaults to
    // ERROR when RUST_LOG is unset. Developers set RUST_LOG by habit and so
    // never see that — but the shipped LaunchAgent sets no environment at all,
    // so an installed user's support log could contain nothing except benign
    // `noq_proto` PTO spam (which that crate emits at error! level), while every
    // diagnostic the daemon already knows how to print stayed invisible. That is
    // exactly backwards for the logs you only ever read after something broke.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new(
                    "info,holoiroh_daemon=debug,iroh_moq=info,noq_proto=warn",
                )
            }),
        )
        .init();
    tracing::info!("holoiroh-daemon starting");

    // Parsed before the single-instance guard below, which needs to know whether this is a
    // `--preflight` run. Parsing is pure and cannot fail destructively, and doing it first also
    // means `--help` and argument errors are reported instead of "another instance is running".
    let cli = Cli::parse();

    // --- single-instance guard, before ANY other startup work (including
    // .env/auth/permission checks below, which are all cheap to redo but
    // pointless if a second instance is about to fail this exact check).
    // See `instance_guard`'s module doc for the live-witnessed failure mode
    // this closes: two daemons racing for `holo serve`'s port, with the
    // loser silently publishing a QR code with no control channel mounted. ---
    //
    // `--preflight` is exempt, and deliberately so: it exists to diagnose a Mac that will not
    // pair, which is exactly when another daemon is likely already running, and refusing to run
    // then would make the diagnostic useless precisely when it is needed. It is safe because it
    // takes nothing contended -- no `holo serve` port, no control channel, no published
    // broadcast; it binds an ephemeral endpoint and opens a second ScreenCaptureKit stream,
    // which macOS supports alongside the first.
    let _instance_guard = match instance_guard::InstanceGuard::acquire() {
        Ok(guard) => Some(guard),
        Err(_) if cli.preflight => {
            eprintln!(
                "[holoiroh-daemon] note: another daemon is already running. Preflight does not \
                 interfere with it -- continuing."
            );
            None
        }
        Err(err) => {
            eprintln!("[holoiroh-daemon] {err}");
            anyhow::bail!("{err}");
        }
    };

    // --- load holoiroh/mac-daemon/.env (gitignored; HAI_API_KEY) into process env, before
    // anything reads it. Missing file is not an error -- `dotenvy::dotenv()` only errors on a
    // malformed .env that IS present; a user relying purely on `~/.holo/.env` (holo login's
    // own output) never needs a local .env at all. ---
    match dotenvy::dotenv() {
        Ok(path) => info!(path = %path.display(), "loaded .env"),
        Err(dotenvy::Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
            // No local .env -- fine, HAI_API_KEY may still come from ~/.holo/.env or an
            // already-exported process env var.
        }
        Err(err) => warn!(error = %err, "failed to parse .env; continuing without it"),
    }

    // --- Holo auth token check, before any other startup work. `holo
    // serve` (mounted below via `HoloBridge::start`) depends on
    // `holo login` having already happened; failing here with a clear
    // instruction is far better than letting `holo serve` fail
    // confusingly (or partially start) later. ---
    match auth::check_holo_token() {
        Ok(token) => info!(source = %token.path().display(), "Holo auth token found"),
        Err(err) => {
            eprintln!("[holoiroh-daemon] {err}");
            eprintln!("  {}", err.remediation());
            anyhow::bail!("Holo auth check failed: {err}");
        }
    }

    // --- macOS permission preflight (Screen Recording + Accessibility).
    // Refuse to start the broadcast with a black/frozen stream or a
    // daemon that can't actually drive the Mac -- report every missing
    // permission at once and exit before any capture/publish work. ---
    let mut preflight = permissions::preflight();
    if std::env::var("HOLOIROH_INPUT_DRY_RUN").as_deref() == Ok("1") {
        preflight
            .missing
            .retain(|permission| *permission != permissions::MissingPermission::Accessibility);
        info!("remote input dry-run enabled -- Accessibility grant is not required");
    }
    if !preflight.is_ok() {
        preflight.report();
        anyhow::bail!(
            "{} macOS permission(s) missing; see instructions above",
            preflight.missing.len()
        );
    }

    // --- iroh endpoint (reads IROH_SECRET if set, otherwise generates a
    // fresh key each run). Built *without* `.with_router()` because we own
    // the `Router` ourselves below, so both `Live`'s protocols (MoQ/gossip)
    // and the control channel's ALPN can be mounted on one shared
    // `Endpoint`/`Router` -- see `Live::register_protocols`'s own doc:
    // "If you already have a router ... skip [`with_router`] and call
    // `Live::register_protocols` on your own `RouterBuilder` instead."
    //
    // NOT `Live::from_env().await?.spawn()` (iroh-live's own convenience
    // path): that always binds BOTH an IPv4 and an IPv6 UDP transport
    // (`Endpoint::builder`'s default), with no override hook. Live-witnessed
    // failure mode this closes: on a Mac with no IPv6 default route (`route
    // -n get -inet6 default` -> "not in table" -- real, not hypothetical;
    // this machine has only a link-local `fe80::` v6 address) but a peer
    // (e.g. an iPhone on cellular) that DOES have a real global IPv6
    // address, iroh's QUIC layer still tries that IPv6 candidate path first
    // and every `sendmsg` on it fails with `HostUnreachable` -- observed
    // repeatedly, one attempt per relay-keepalive-ish interval, for 60+
    // seconds before anything gets through on the working IPv4/relay path.
    // From the phone's side this looks exactly like "connected but hangs,
    // no errors": the control channel's `session established` log line
    // fires, but the greeting/every reply is queued behind a doomed IPv6
    // send. Building the endpoint by hand (`iroh_live::util::secret_key_from_env`
    // + `Endpoint::builder`, matching `Live::from_env`'s own implementation)
    // so `clear_ip_transports()` can drop the IPv6 UDP socket entirely when
    // this machine has no v6 route -- skipping the dead path outright
    // instead of waiting out iroh's own path-probing/migration timeout. ---
    let secret_key = persistent_secret_key()?;
    // mDNS address lookup, ADDED to the N0 preset rather than replacing it, so a phone on
    // cellular still reaches this Mac through the relay exactly as before.
    //
    // What it fixes: `presets::N0` gives pkarr publish + n0 DNS resolve + relay and nothing else,
    // and the publisher defaults to advertising RELAY addresses only. Resolving the daemon's node
    // id therefore hands the phone a relay URL, so a phone and a Mac in the same room begin every
    // session with their packets crossing a datacentre, and only fall back to a local path once
    // in-band NAT-traversal candidate exchange discovers one. `lan_discovery_probe` measures the
    // difference on this machine: stock reaches a direct path 1.36s after connecting, with mDNS
    // 198ms, and the connect itself drops from 842ms to 96ms.
    //
    // Safe in a way that publishing the same addresses via pkarr would not be: mDNS records are
    // link-local and never leave the network the two devices are already sharing.
    let mut endpoint_builder = iroh::Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key(secret_key)
        .address_lookup(iroh_mdns_address_lookup::MdnsAddressLookup::builder());
    if has_ipv6_default_route() {
        info!("IPv6 default route present -- binding both IPv4 and IPv6 transports");
    } else {
        warn!(
            "no IPv6 default route on this machine -- binding IPv4 only (a v6-capable peer would otherwise stall the control channel retrying doomed IPv6 sends; see main.rs's iroh-endpoint-construction comment)"
        );
        endpoint_builder = endpoint_builder
            .clear_ip_transports()
            .bind_addr("0.0.0.0:0")
            .context("failed to configure IPv4-only iroh transport")?;
    }
    let endpoint = endpoint_builder.bind().await?;
    let control_signing_key = Arc::new(endpoint.secret_key().clone());
    let live = Live::builder(endpoint).spawn();
    info!(id = %live.endpoint().id(), "endpoint ready");

    if cli.preflight {
        return run_preflight(&live, cli.display).await;
    }

    // --- metadata-only local audit log (Project Aro PRD row P0-12; see `audit_log`'s module
    // doc). Best-effort, matching `holo_bridge`'s own degrade-don't-crash posture: a disk/
    // permissions problem creating `~/.holoiroh/` must not prevent the daemon from publishing
    // its broadcast or accepting control-channel connections -- those still work (minus audit
    // logging) even if this fails. Constructed once here and shared (via `Arc`) into whichever
    // `ControlChannel` constructor runs below. ---
    let audit_logger = match audit_log::AuditLogger::from_env() {
        Ok(logger) => {
            info!(path = %logger.path().display(), "audit log ready");
            Arc::new(logger)
        }
        Err(err) => {
            warn!(error = %err, "audit log failed to initialize -- control channel will run without task audit logging");
            // Falls back to an in-memory-only path resolution failure state: `AuditLogger::new`
            // only fails on `create_dir_all`, so retrying the same path on every `append` call
            // would fail identically every time. Rather than making every downstream call site
            // handle an `Option<Arc<AuditLogger>>`, construct a logger pointed at a path under
            // the OS temp dir as a last-resort fallback so `append`'s own per-call error handling
            // (already required for real disk-full/permissions races) is the only error path
            // anything downstream needs to handle -- this is strictly a "logging is
            // best-effort, never load-bearing" daemon, so a temp-dir fallback location is an
            // acceptable degradation, not a silent data-integrity issue.
            let fallback = std::env::temp_dir().join("holoiroh-audit-fallback.log");
            Arc::new(audit_log::AuditLogger::new(&fallback).unwrap_or_else(|_| {
                panic!(
                    "audit log fallback path {} must be constructible",
                    fallback.display()
                )
            }))
        }
    };

    // --- Aro Private mode: local on-device inference via a `llama-server`
    // (llama.cpp) subprocess serving the Holo3.1 vision model, bound to
    // 127.0.0.1 only. This is the alpha's ONLY inference backend -- the
    // build carries no cloud inference code path (Project Aro PRD P0-11) --
    // and `holo serve` (started just below) is pointed at it via
    // `--base-url` + `HAI_AGENT_RUNTIME_BASE_URL` (see `local_model` and
    // `holo_bridge::process` module docs for why that env var, not
    // `HAI_BASE_URL`, is the one that redirects inference and drops the
    // hosted API key).
    //
    // Best-effort + degrade-don't-crash, matching `holo_bridge`'s posture:
    // spawning `llama-server` loads the ~21 GB GGUF and can take minutes, so
    // a failure here (binary missing, model not cached, RAM pressure) is
    // logged and the daemon still publishes its broadcast; the control
    // channel then surfaces "inference unavailable" rather than the process
    // dying. The local server, when it comes up, is held for the daemon's
    // lifetime and shut down in the cleanup sequence below. ---
    let (local_model_server, local_llama_proxy) = if local_model_enabled() {
        let config = local_model::LocalModelConfig::from_env();
        if config.port == holo_serve_port() {
            warn!(
                port = config.port,
                "local model port equals holo serve port; they must differ (two distinct listeners) -- set HOLOIROH_LOCAL_MODEL_PORT or HOLOIROH_HOLO_PORT"
            );
        }
        info!(
            upstream_base_url = %config.base_url(),
            model = %config.model_hf_repo,
            max_tokens = config.max_tokens,
            "starting local llama-server (Aro Private mode; loading the model can take minutes)"
        );
        match local_model::LocalModelServer::spawn(config).await {
            Ok(server) => {
                info!(pid = ?server.pid(), upstream_base_url = %server.base_url(), "local llama-server ready");
                match local_llama_proxy::LocalLlamaProxy::spawn_for_config(server.config()).await {
                    Ok(proxy) => {
                        info!(proxy_base_url = %proxy.base_url(), "local llama proxy ready");
                        (Some(server), Some(proxy))
                    }
                    Err(err) => {
                        warn!(error = %err, "local llama proxy failed to start -- disabling the local inference backend");
                        if let Err(shutdown_err) = server.shutdown().await {
                            warn!(error = %shutdown_err, "local llama-server cleanup after proxy failure failed");
                        }
                        (None, None)
                    }
                }
            }
            Err(err) => {
                warn!(error = %err, "local llama-server failed to start -- holo serve will have no local inference backend");
                (None, None)
            }
        }
    } else {
        info!(
            "HOLOIROH_LOCAL_MODEL disabled -- not starting a local llama-server; holo serve uses its configured backend"
        );
        (None, None)
    };
    let primary_target = local_llama_proxy
        .as_ref()
        .map(|proxy| holo_bridge::InferenceTarget {
            base_url: proxy.base_url(),
            model: None,
            label: "local llama-server via constrained loopback proxy".to_string(),
        });

    // --- rate-limit FALLBACK backend (tinfoil kimi-k2-6, a vision model, via
    // a loopback auth-injecting proxy -- see tinfoil_proxy.rs for why a proxy
    // is the only workable auth path). Configured whenever TINFOIL_API_KEY is
    // present in the environment (mac-daemon/.env), EXCEPT in local (no-cloud)
    // mode, where failing over to a cloud endpoint would defeat the mode. When
    // the hosted H backend rate-limits (its 429s surface as `holo serve`'s
    // generic "agent backend error"), the bridge switches `holo serve` onto
    // this backend and retries the failed turn once automatically; after a
    // cooldown (default 10 min) the next turn probes the hosted path again. ---
    let tinfoil_key = std::env::var("TINFOIL_API_KEY")
        .ok()
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty());
    let tinfoil_client = match tinfoil_key {
        Some(key) => match tokio::time::timeout(
            TINFOIL_INIT_TIMEOUT,
            tinfoil_client::TinfoilClient::new(key),
        )
        .await
        {
            Ok(Ok(client)) => {
                let client = Arc::new(client);
                info!(host = %client.base_url(), "Tinfoil enclave attestation verified");
                Some(client)
            }
            Ok(Err(err)) => {
                warn!(error = %format!("{err:#}"), "Tinfoil attestation failed; all Tinfoil egress disabled and daemon startup continues");
                None
            }
            Err(_) => {
                warn!(
                    timeout_seconds = TINFOIL_INIT_TIMEOUT.as_secs(),
                    "Tinfoil initialization timed out; all Tinfoil egress disabled and daemon startup continues"
                );
                None
            }
        },
        None => None,
    };
    let clarify_config = tinfoil_client.clone().map(clarify::ClarifyConfig::new);
    let tinfoil_client_for_control_channel = tinfoil_client.clone();
    match &clarify_config {
        Some(cfg) => info!(model = %cfg.model(), "clarifying-questions inference enabled"),
        None => info!("clarifying-questions inference disabled (no TINFOIL_API_KEY)"),
    }

    match tokio::task::spawn_blocking(tmux::ensure_session).await {
        Ok(state) => info!(
            session = tmux::SESSION_NAME,
            usable_by_agent = state.agent_can_use_it(),
            ?state,
            "shared terminal session ready for agent CLI work"
        ),
        Err(err) => {
            warn!(error = %err, "ensuring the shared tmux session panicked; terminal guidance falls back to plain-terminal wording")
        }
    }
    // Underscore-named (not bare `_`): the binding must LIVE until main
    // returns -- dropping it aborts the proxy task and every fallback
    // inference call with it. A bare `_` pattern would drop it right here.
    let (_tinfoil_proxy_handle, fallback_target) = if local_model_server.is_some() {
        info!("local (no-cloud) mode active -- tinfoil rate-limit fallback disabled by design");
        (None, None)
    } else if let Some(client) = tinfoil_client.clone() {
        let upstream = client.base_url();
        let model = std::env::var("HOLOIROH_FALLBACK_MODEL")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "kimi-k2-6".to_string());
        match tinfoil_proxy::TinfoilProxy::spawn(client).await {
            Ok(proxy) => {
                let target = holo_bridge::InferenceTarget {
                    // OpenAI-compatible routes live under /v1 upstream; the proxy
                    // forwards paths verbatim, so point holo at <proxy>/v1 exactly
                    // like the local llama-server convention.
                    base_url: format!("{}/v1", proxy.local_url()),
                    model: Some(model.clone()),
                    label: format!("{model} ({upstream})"),
                };
                info!(model = %model, upstream = %upstream, proxy = %proxy.local_url(),
                    "tinfoil rate-limit fallback backend configured");
                (Some(proxy), Some(target))
            }
            Err(err) => {
                warn!(error = %format!("{err:#}"),
                    "tinfoil fallback proxy failed to start -- no rate-limit fallback this run");
                (None, None)
            }
        }
    } else {
        info!("TINFOIL_API_KEY not set -- no rate-limit fallback backend");
        (None, None)
    };
    let fallback_cooldown = std::time::Duration::from_secs(
        std::env::var("HOLOIROH_FALLBACK_COOLDOWN_SECS")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            // 30 min: each cooldown expiry probes the hosted backend with a
            // real turn (a ~15s failed-then-retried task when it is STILL
            // rate-limited), so probe sparingly -- the fallback is fully
            // capable in the meantime.
            .unwrap_or(1800),
    );

    // --- best-effort holo_bridge startup. A missing/unhealthy `holo`
    // binary must not prevent the daemon from publishing its broadcast or
    // accepting control-channel connections (which still work for
    // ack/status/error even without a bridge, e.g. surfacing "holo serve
    // unavailable" as a status message) -- so this is logged, not
    // propagated with `?`. ---
    let (bridge_events_tx, _bridge_events_rx) = mpsc::unbounded_channel();
    let health_check_shutdown = tokio_util::sync::CancellationToken::new();
    let bridge = match HoloBridge::start(
        holo_bin(),
        holo_serve_port(),
        primary_target,
        fallback_target,
        fallback_cooldown,
        bridge_events_tx,
    )
    .await
    {
        Ok(bridge) => {
            info!(pid = ?bridge.holo_serve_pid().await, "holo_bridge started");
            let bridge = Arc::new(bridge);
            // Failover backref: the control bridge detects backend-error turns but the
            // process-swap machinery lives on HoloBridge -- see HoloControlBridge::attach_bridge.
            bridge.control.attach_bridge(Arc::downgrade(&bridge));
            // Ongoing supervisor: `HoloBridge::start`'s own health wait only runs once, at
            // startup. This background loop keeps polling for the rest of the daemon's
            // lifetime and restarts `holo serve` on crash -- see `holo_bridge::health`'s
            // module doc for why this can never reach into the iroh P2P session.
            tokio::spawn(holo_bridge::health::run_health_check_loop(
                bridge.clone(),
                health_check_shutdown.clone(),
            ));
            // Autonomous self-correction backstop: watches the running turn for a genuine
            // stall and nudges it to fix its own mistake instead of sitting stuck. See
            // `holo_bridge::stall_watchdog`'s module doc. Shares the health-check loop's own
            // shutdown token -- both are daemon-lifetime background supervisors over the same
            // bridge, so one cancellation stops both cleanly.
            tokio::spawn(holo_bridge::stall_watchdog::run_stall_watchdog_loop(
                bridge.clone(),
                health_check_shutdown.clone(),
            ));
            // Lock/login-screen visibility: watches macOS's own secure-input signal and tells
            // the phone when the black rectangle over the video is the login/lock password
            // field (an OS security boundary, not a bug) rather than leaving it unexplained.
            // See `holo_bridge::secure_input_watchdog`'s module doc. Same shared shutdown token
            // as the other daemon-lifetime background supervisors above.
            tokio::spawn(
                holo_bridge::secure_input_watchdog::run_secure_input_watchdog_loop(
                    bridge.clone(),
                    health_check_shutdown.clone(),
                ),
            );
            // Cooperative auto-yield: step the agent aside while the user is
            // actively using the Mac, resume when they go idle (see
            // `crate::auto_yield`). Starts its own physical-input CGEventTap;
            // degrades to inactive if Input-Monitoring permission is absent.
            auto_yield::spawn_monitor(bridge.clone());
            Some(bridge)
        }
        Err(err) => {
            // `{err:#}` (anyhow's full context chain), not `%err`/`{err}` (outermost message
            // only) -- the outer "failed to start holo serve" alone swallowed the actionable
            // root cause (e.g. the agent-card 401, or a bind failure) every time this fired.
            warn!(error = %format!("{err:#}"), "holo_bridge failed to start -- control channel will run without it");
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
        // HOLOIROH_PIN pins a STABLE pairing PIN across daemon restarts. Without it the
        // per-run random PIN silently invalidates the iOS app's saved connection profiles
        // (the sqlite "Dev Mac" profile stores ticket + PIN; the ticket is already stable
        // via ~/.holoiroh/iroh_secret, but the PIN changed every run) -- an allowlisted
        // device never re-sends the PIN so this only bites fresh installs/devices, which
        // is exactly when it's most confusing. Env var, not a CLI flag, so the PIN never
        // shows up in `ps` output.
        match std::env::var("HOLOIROH_PIN") {
            Ok(v) if !v.trim().is_empty() => {
                info!("using stable pairing PIN from HOLOIROH_PIN");
                Some(v.trim().to_string())
            }
            _ => Some(generate_default_pin()),
        }
    };

    // --- build the shared Router: Live's own protocols (MoQ, gossip if
    // enabled) plus the control channel's ALPN, all on `live.endpoint()`.
    // See control_channel.rs's module doc for why this is "a second
    // logical stream on the same iroh QUIC connection" in iroh's
    // connection-per-ALPN model. ---
    let router_builder = Router::builder(live.endpoint().clone());
    let router_builder = live.register_protocols(router_builder);
    // The daemon's OWN drift-proof (node-id-only) ticket, handed to every
    // accepted control connection as a CurrentTicket so a client can refresh a
    // stored default that went stale on identity rotation. Node-id-only (not
    // live.endpoint().addr()'s address-hinted form) so it matches the iOS app's
    // stored constant format and only differs when the identity key actually
    // changed -- not on every restart's address churn.
    let daemon_control_ticket: Arc<str> = Arc::from(
        LiveTicket::new(EndpointAddr::from(live.endpoint().id()), BROADCAST_NAME)
            .to_string()
            .as_str(),
    );
    let executor_approval_store =
        Arc::new(std::sync::Mutex::new(approval::ApprovalStore::default()));
    let typed_action_executor = Arc::new(std::sync::Mutex::new(
        action_executor::DaemonActionExecutor::new(
            executor_approval_store.clone(),
            approval::DEFAULT_APPROVAL_CAPACITY,
        )
        .with_audit(audit_logger.clone()),
    ));
    let router_builder = match bridge.clone() {
        Some(bridge) => {
            let control = match pin.clone() {
                Some(pin) => ControlChannel::with_auth(
                    bridge,
                    cli.execution_mode,
                    control_signing_key.clone(),
                    pin,
                    audit_logger.clone(),
                    daemon_control_ticket.clone(),
                    clarify_config.clone(),
                    tinfoil_client_for_control_channel.clone(),
                ),
                None => ControlChannel::new(
                    bridge,
                    cli.execution_mode,
                    control_signing_key.clone(),
                    audit_logger.clone(),
                    daemon_control_ticket.clone(),
                    clarify_config.clone(),
                    tinfoil_client_for_control_channel.clone(),
                ),
            }
            .with_action_executor(typed_action_executor.clone());
            control.register_protocols(router_builder)
        }
        None => {
            // Loud, not `info!`: this daemon is about to publish a fully
            // valid-looking QR code/ticket/phrase that a phone CAN pair
            // with, but whose control-channel dial will then fail ALPN
            // negotiation on every attempt (iroh error 120, "peer doesn't
            // support any known protocol") -- indistinguishable from a
            // crash to the end user. The `instance_guard` module prevents
            // the most common cause (a second daemon losing the `holo
            // serve` port race) from reaching this branch at all; this
            // warning covers every other `HoloBridge::start` failure mode
            // (see the `Err(err)` arm above for the actual cause logged
            // moments earlier).
            warn!(
                "control channel NOT mounted (no holo_bridge available) -- the QR code about to be printed will pair successfully but EVERY control-channel connection will then fail; see the 'holo_bridge failed to start' warning above for the root cause"
            );
            router_builder
        }
    };
    let router = router_builder.spawn();

    // Broadcast with the ScreenCaptureKit video source attached -- no audio
    // source yet. `capture::setup_screen_video` resolves `--display` (or the
    // primary display when omitted) and calls `broadcast.video().set_source(..)`
    // on our behalf.
    //
    // Encoder selection (Project Aro PRD OQ-5, "H.264-over-iroh"; see
    // TRANSPORT_ADR.md): pick the *hardware* H.264 encoder when one is
    // available rather than hardcoding software `VideoCodec::H264` (openh264).
    // `VideoCodec::best_available()` prefers hardware over software and, on this
    // macOS build (iroh-live's default features include `videotoolbox`), returns
    // `VtbH264` -- Apple VideoToolbox producing standard H.264/AVC
    // (`kCMVideoCodecType_H264 = 'avc1'`, decodable unchanged by the iOS
    // `AVSampleBufferDisplayLayer` path). This is exactly the "VideoToolbox-
    // encoded frames over iroh's QUIC/MoQ transport" OQ-5 names as the primary
    // candidate, and matches iroh-live's own reference CLI, which defaults the
    // codec via `VideoCodec::parse_or_best(None)` -> `best_available()`. The
    // wire codec is H.264 either way, so the fallback to software openh264 (when
    // no hardware encoder is compiled in / available) is a graceful CPU-cost
    // degradation, never a format change that would break the iOS decoder.
    let video_codec = VideoCodec::best_available().unwrap_or(VideoCodec::H264);
    info!(
        codec = ?video_codec,
        hardware = video_codec.is_hardware(),
        "selected H.264 video encoder for the iroh/MoQ broadcast (OQ-5: H.264-over-iroh)"
    );
    let broadcast = LocalBroadcast::new();
    capture::setup_screen_video(&broadcast, cli.display, video_codec, &[VideoPreset::P720])?;

    // --- publish, then present the shareable ticket as a scannable QR code
    // AND its raw text (per PAIRING.md's "QR + short-phrase pairing" design).
    // The QR lets the iOS app scan the ticket instead of the operator
    // retyping a long string; the raw text below it is the fallback for
    // terminals whose font distorts block-character QR codes. ---
    live.publish(BROADCAST_NAME, &broadcast).await?;
    let ticket = LiveTicket::new(live.endpoint().addr(), BROADCAST_NAME);
    let ticket_str = ticket.to_string();
    // Ticket QR + raw text + verification phrase (SAS). The iOS app derives the SAME phrase from
    // the scanned ticket (byte-identical SHA-256 + wordlist, see ios/PAIRING_PHRASE.md) and asks
    // the user to confirm the two match -- so a MITM who substituted the QR (and thus the ticket)
    // produces a different phrase here than the phone shows. `print_pairing_block` is reused on
    // each `--rotate-every` rotation tick below so the printouts are identical.
    print_pairing_block(&ticket_str, "pairing");
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
    // launchd/Docker stop, both of which send SIGTERM by default. REQUIRES the `ctrlc` crate's
    // `termination` feature (Cargo.toml) -- without it, `set_handler` only ever catches SIGINT,
    // SIGTERM is silently ignored by the process's default disposition (terminate immediately,
    // no handler run at all), and this whole shutdown sequence below never executes. Witnessed
    // live as the real cause of a recurring "daemon hangs, no QR" report: every `kill`/closed-
    // terminal stop of a prior daemon run left `holo serve` + `hai-agent-runtime` orphaned and
    // still holding port 8765/18795, so the NEXT launch attempt raced already-squatted ports.
    // `ctrlc`'s handler runs on its own dedicated OS thread and is not `async`, so it only flips
    // a channel to wake the async task below rather than doing any cleanup itself.
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let shutdown_tx = std::sync::Mutex::new(Some(shutdown_tx));
    ctrlc::set_handler(move || {
        if let Some(tx) = shutdown_tx.lock().unwrap().take() {
            let _ = tx.send(());
        }
    })
    .context("failed to register SIGINT/SIGTERM handler")?;

    // Wait for shutdown, racing it against the optional `--rotate-every` rotation ticker. When
    // the ticker fires, re-print the pairing block (QR + ticket + verification phrase) so a stale
    // QR screenshot stops matching what the operator is now reading off. This re-prints the
    // *current* ticket (see `Cli::rotate_every`'s doc for why a full fresh-keypair rotation is a
    // separate, larger step); the phrase re-renders identically since it is derived from the
    // ticket. When `rotate_every` is `None`, `rotate_ticker` never fires and this behaves exactly
    // like the plain `shutdown_rx.await` it replaces.
    let mut rotate_ticker = cli.rotate_every.map(|interval| {
        let mut t = tokio::time::interval(interval);
        // The first `.tick()` on a fresh `interval` completes immediately; skip it so we don't
        // re-print the pairing block a millisecond after the startup print. `MissedTickBehavior::
        // Skip` also means a rotation missed under load is dropped, not fired in a catch-up burst.
        t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        t
    });
    let mut shutdown_rx = shutdown_rx;
    loop {
        match &mut rotate_ticker {
            Some(ticker) => {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    _ = ticker.tick() => {
                        // Skip the immediate first tick (see above), then re-print on each real one.
                        static FIRST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
                        if FIRST.swap(false, std::sync::atomic::Ordering::Relaxed) {
                            continue;
                        }
                        info!("rotate-every: re-printing pairing block");
                        print_pairing_block(&ticket_str, "rotated");
                    }
                }
            }
            None => {
                let _ = (&mut shutdown_rx).await;
                break;
            }
        }
    }

    info!("shutdown signal received, cleaning up");
    health_check_shutdown.cancel();
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
    if let Some(proxy) = local_llama_proxy {
        info!(proxy_base_url = %proxy.base_url(), "shutting down local llama proxy");
        if let Err(err) = proxy.shutdown().await {
            warn!(error = %err, "local llama proxy shutdown error");
        }
    }
    // Stop the local `llama-server` AFTER `holo serve` (which was pointed at it): the inference
    // backend outliving nothing means no orphaned 21 GB process is left holding memory. Owned
    // directly (not behind an `Arc`), so this always gets the graceful awaited SIGTERM-then-kill
    // path; `Drop` (+ `kill_on_drop`) is only the safety net for a panic before we reach here.
    if let Some(server) = local_model_server {
        info!(pid = ?server.pid(), "shutting down local llama-server");
        if let Err(err) = server.shutdown().await {
            warn!(error = %err, "local llama-server shutdown error");
        }
    }
    live.shutdown().await;
    Ok(())
}
