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

mod agent_guidance;
mod allowlist;
mod audit_log;
mod auto_yield;
mod remote_input;
mod user_activity;
mod auth;
mod capture;
mod control_channel;
mod duration;
// NOTE: `executor` (the ComputerUseExecutor abstraction seam, PRD 7.3) is deliberately declared
// only in `lib.rs`, not here. It is an available seam consumed by `examples/executor_probe.rs`
// via the `holoiroh_daemon` lib crate; wiring the live daemon's control path to route through it
// (rather than calling `HoloBridge` directly, as `main.rs` does today) is a separate follow-on and
// is intentionally out of this pass's scope. Declaring `mod executor;` in the binary target too
// would compile the whole seam as dead code here (25 warnings), since nothing in `main.rs`
// references it yet -- so it lives in the lib target only until that wiring lands.
mod frontmost_app;
mod holo_bridge;
mod instance_guard;
mod limits;
mod local_model;
mod router;
mod env_context;
mod task_fsm;
mod tinfoil_proxy;
mod pairing_phrase;
mod permissions;
mod policy;
mod process_awareness;
mod registry;
mod sensitive_categories;
mod task_state;

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

    /// Re-print the pairing ticket + verification phrase on a fixed interval while the daemon
    /// keeps running (e.g. `30m`, `2h`, `1h30m`), so a stale QR screenshot stops being the one
    /// the operator is reading off. See `holoiroh/mac-daemon/PAIRING.md`'s "Ticket rotation"
    /// section. NOTE: this re-prints the *current* ticket -- a full fresh-keypair-per-tick
    /// identity rotation (which would invalidate old tickets entirely) requires tearing down and
    /// rebuilding the iroh `Live` session mid-run and is documented there as a separate, larger
    /// step. Rotation-on-restart already happens implicitly (fresh keypair per process start
    /// when `IROH_SECRET` is unset), and the device allowlist gives device-level rotation
    /// protection regardless.
    #[arg(long, value_parser = duration::parse_rotate_duration)]
    rotate_every: Option<std::time::Duration>,
}

/// `holo` CLI executable used to spawn `holo serve` (see
/// `holo_bridge::process`). Overridable via `HOLOIROH_HOLO_BIN` so a dev
/// machine can point at a non-`PATH` binary without editing source.
///
/// Falls back to `~/.holo/bin/holo` (the path `holo login`'s own installer
/// writes -- see `auth.rs`'s module doc) when bare `"holo"` is not resolvable
/// on `PATH`, rather than always emitting the literal string `"holo"` and
/// letting `tokio::process::Command::spawn` fail with an opaque
/// `No such file or directory (os error 2)` -- witnessed live: `holo` is
/// genuinely absent from a plain non-interactive shell's `PATH` (it's only on
/// the user's own interactive shell rc), so this fallback is not hypothetical.
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
/// does for a bare (non-slash-containing) program name -- used only to decide
/// whether [`holo_bin`]'s `~/.holo/bin/holo` fallback is needed, not as a
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

/// Print the shareable ticket as a QR code, its raw text, and its verification phrase -- the
/// exact block re-emitted both at startup and on each `--rotate-every` rotation tick, so the two
/// are byte-identical. `context` is a short label (e.g. "pairing" / "rotated") shown in the
/// header line so the operator can tell a rotation apart from the initial print.
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
/// characters, per PAIRING.md's terminal-rendering design. Best-effort: a
/// QR-construction failure (e.g. the ticket string somehow exceeding QR
/// capacity) is logged and skipped rather than aborting startup -- the raw
/// ticket text printed alongside it is always the authoritative fallback.
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
/// `holo serve` at it, versus leaving `holo serve` on the hosted Holo3 API (via `HAI_API_KEY`).
///
/// Defaults to **off** (hosted API) as of this build: starting the local `llama-server` means
/// loading a 21GB model, which can take minutes with no output before the daemon prints
/// anything -- witnessed live as a silent, indistinguishable-from-hung startup on a plain
/// `holoiroh-daemon` invocation with no env vars set (the exact symptom reported: "just hangs,
/// no QR code shows up"). Set `HOLOIROH_LOCAL_MODEL=1` (or `true`/`yes`) to opt IN to local
/// inference (Project Aro PRD P0-11's no-cloud-path mode) once that tradeoff is wanted again.
/// The daemon's iroh identity key, STABLE across restarts.
///
/// `IROH_SECRET` env wins when set (iroh-live's own convention, unchanged).
/// Otherwise the key is loaded from -- or first generated into --
/// `~/.holoiroh/iroh_secret` (hex, 0600, same config dir as
/// `allowlist.json`). Without this, every daemon restart minted a fresh
/// random identity (`SecretKey::generate` inside
/// `iroh_live::util::secret_key_from_env`), which changes the node id and
/// therefore the pairing ticket -- silently invalidating every saved
/// connection profile in the iOS app and forcing a QR re-scan per restart.
fn persistent_secret_key() -> anyhow::Result<iroh::SecretKey> {
    if std::env::var("IROH_SECRET").is_ok() {
        return Ok(iroh_live::util::secret_key_from_env()?);
    }
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME not set; cannot locate ~/.holoiroh/iroh_secret"))?;
    let dir = home.join(".holoiroh");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating {}", dir.display()))?;
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
    tracing_subscriber::fmt::init();
    tracing::info!("holoiroh-daemon starting");

    // --- single-instance guard, before ANY other startup work (including
    // .env/auth/permission checks below, which are all cheap to redo but
    // pointless if a second instance is about to fail this exact check).
    // See `instance_guard`'s module doc for the live-witnessed failure mode
    // this closes: two daemons racing for `holo serve`'s port, with the
    // loser silently publishing a QR code with no control channel mounted. ---
    let _instance_guard = match instance_guard::InstanceGuard::acquire() {
        Ok(guard) => guard,
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

    let cli = Cli::parse();

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
    let preflight = permissions::preflight();
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
    let mut endpoint_builder =
        iroh::Endpoint::builder(iroh::endpoint::presets::N0).secret_key(secret_key);
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
    let live = Live::builder(endpoint).spawn();
    info!(id = %live.endpoint().id(), "endpoint ready");

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
            Arc::new(
                audit_log::AuditLogger::new(&fallback)
                    .unwrap_or_else(|_| panic!("audit log fallback path {} must be constructible", fallback.display())),
            )
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
    let local_model_server = if local_model_enabled() {
        let config = local_model::LocalModelConfig::from_env();
        if config.port == holo_serve_port() {
            warn!(
                port = config.port,
                "local model port equals holo serve port; they must differ (two distinct listeners) -- set HOLOIROH_LOCAL_MODEL_PORT or HOLOIROH_HOLO_PORT"
            );
        }
        info!(
            base_url = %config.base_url(),
            model = %config.model_hf_repo,
            "starting local llama-server (Aro Private mode; loading the model can take minutes)"
        );
        match local_model::LocalModelServer::spawn(config).await {
            Ok(server) => {
                info!(pid = ?server.pid(), base_url = %server.base_url(), "local llama-server ready");
                Some(server)
            }
            Err(err) => {
                warn!(error = %err, "local llama-server failed to start -- holo serve will have no local inference backend");
                None
            }
        }
    } else {
        info!("HOLOIROH_LOCAL_MODEL disabled -- not starting a local llama-server; holo serve uses its configured backend");
        None
    };
    // The PRIMARY inference backend to hand `holo serve`: the local server's
    // when it came up, else `None` (holo serve keeps its own configured hosted
    // backend, which in a correctly-configured local-only alpha means it will
    // fail to reach a model and the control channel reports that, rather than
    // silently reaching a cloud endpoint).
    let primary_target = local_model_server.as_ref().map(|s| holo_bridge::InferenceTarget {
        base_url: s.base_url(),
        // llama-server serves whatever model it loaded regardless of the
        // requested name; no override needed.
        model: None,
        label: "local llama-server".to_string(),
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
    // Underscore-named (not bare `_`): the binding must LIVE until main
    // returns -- dropping it aborts the proxy task and every fallback
    // inference call with it. A bare `_` pattern would drop it right here.
    let (_tinfoil_proxy_handle, fallback_target) = if local_model_server.is_some() {
        info!("local (no-cloud) mode active -- tinfoil rate-limit fallback disabled by design");
        (None, None)
    } else if let Some(key) = tinfoil_key {
        let upstream = std::env::var("HOLOIROH_FALLBACK_UPSTREAM")
            .ok()
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| tinfoil_proxy::DEFAULT_UPSTREAM.to_string());
        let model = std::env::var("HOLOIROH_FALLBACK_MODEL")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "kimi-k2-6".to_string());
        match tinfoil_proxy::TinfoilProxy::spawn(&upstream, key).await {
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
    let router_builder = match bridge.clone() {
        Some(bridge) => {
            let control = match pin.clone() {
                Some(pin) => ControlChannel::with_auth(
                    bridge,
                    pin,
                    audit_logger.clone(),
                    daemon_control_ticket.clone(),
                ),
                None => ControlChannel::new(bridge, audit_logger.clone(), daemon_control_ticket.clone()),
            };
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
    capture::setup_screen_video(
        &broadcast,
        cli.display,
        video_codec,
        &[VideoPreset::P720],
    )?;

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
