//! Spawns and supervises `holo serve` as a managed child process, and recovers the bearer
//! token it prints to stderr on startup.
//!
//! ## Source grounding
//!
//! Verified directly against `hcompai/holo-desktop-cli` source
//! (`src/holo_desktop/cli/serve.py`, commit reachable from `main` as of 2026-07-17):
//!
//! - `holo serve` binds `127.0.0.1:<port>` (default port 18794, `A2A_DEFAULT_PORT` in
//!   `serve.py`) and serves both the A2A JSON-RPC surface and a plain `/health` route.
//! - Every route except `/health` requires `Authorization: Bearer <token>`
//!   (`BearerAuthMiddleware` in `serve.py`).
//! - The token comes from the `HOLO_AUTH_TOKEN` env var if set (`ServeSettings.auth_token`
//!   in `settings.py`); otherwise `serve()` generates one with
//!   `secrets.token_urlsafe(32)` and **only ever surfaces it by printing it to stderr**:
//!   ```text
//!   holo serve · v<version>
//!     http://127.0.0.1:<port>/a2a
//!     export HOLO_AUTH_TOKEN=<token>
//!     Ctrl+C to stop
//!   ```
//!   There is no token file, no `/token` endpoint, and no other way for a co-process to
//!   recover a generated token. Because parsing stderr text for a secret is fragile, this
//!   module instead **always sets `HOLO_AUTH_TOKEN` itself** before spawning, generating a
//!   fresh random token daemon-side. This sidesteps stderr-scraping entirely and is strictly
//!   more robust than depending on the printed line's format not changing across releases.
//!   (The stderr-parsing fallback path is kept, gated off by default, only for the case where
//!   an operator points this daemon at a `holo serve` invocation it did not itself launch.)
//! - Health check: `GET /health` (no auth) returns
//!   `{"service": "holo-desktop", "status": "ok", "version": "<semver>"}` once the
//!   underlying `hai-agent-runtime` binary has been spawned/attached by `HoloExecutor.startup()`
//!   and the Starlette app has finished its lifespan startup. Before that, connections are
//!   simply refused (nothing is bound yet).

use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::time::{Instant, sleep};

use crate::holo_bridge::a2a_client::A2aClient;

/// Process-wide guard ensuring at most one `holo serve` child is ever tracked as running by
/// this daemon at a time. [`HoloServeProcess::spawn`] refuses to spawn a second child while this
/// is `true`. Held via an owned [`GuardClaim`] token rather than raw stores, so release is tied
/// to ownership: only the object that acquired the claim can release it, and it does so exactly
/// once. This exists because the earlier raw-`store(false)` design had two real bugs (witnessed
/// live via `holo_bridge::health`'s "restart failed: failed to respawn holo serve" loop, and by
/// `examples/serve_guard_probe.rs`):
///
/// 1. **Restart could never succeed.** `HoloBridge::restart_process` spawned the NEW child while
///    the old (dead-child) `HoloServeProcess` still held the guard -- `compare_exchange` failed
///    every time, deterministically, so the health loop errored every tick forever.
/// 2. **The old process's `Drop` released the new process's guard.** `Drop` did an unconditional
///    `store(false)`; after a (hypothetically successful) restart replaced the old object, the
///    old `Drop` would clear the claim the NEW child had just acquired, re-opening the
///    double-spawn hole the guard exists to close.
///
/// With [`GuardClaim`], the restart path disarms the old process (drops its claim) before
/// spawning the replacement, and a claim-less process's `Drop` cannot touch the flag at all.
static HOLO_SERVE_RUNNING: AtomicBool = AtomicBool::new(false);

/// Owned claim on [`HOLO_SERVE_RUNNING`]. Acquiring ([`GuardClaim::try_acquire`]) atomically
/// flips the flag `false -> true`; dropping the claim releases it (`-> false`), exactly once,
/// and only for the claim that actually holds it. Zero-sized -- the token IS the ownership.
///
/// Public (not just `pub(crate)`) so `examples/serve_guard_probe.rs` can witness the
/// acquire/second-acquire-fails/release/re-acquire lifecycle and the restart-ordering
/// (disarm-old-then-acquire-new leaves the flag held by the new claim; dropping the disarmed
/// old is a no-op) against the REAL static, per this repo's probe-witness rule.
#[derive(Debug)]
pub struct GuardClaim(());

impl GuardClaim {
    /// Atomically claim the guard. `None` if another live claim already holds it.
    /// `compare_exchange` makes the check-and-set atomic so two concurrent callers can't both
    /// observe `false` and both proceed.
    pub fn try_acquire() -> Option<Self> {
        HOLO_SERVE_RUNNING
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
            .then_some(GuardClaim(()))
    }
}

impl Drop for GuardClaim {
    fn drop(&mut self) {
        HOLO_SERVE_RUNNING.store(false, Ordering::SeqCst);
    }
}

/// Env var `holo serve` reads for its bearer token (`ServeSettings.auth_token` /
/// `A2A_TOKEN_ENV` in `serve.py`). Setting this ourselves before spawn means we never have
/// to scrape stderr for a generated token.
pub const HOLO_AUTH_TOKEN_ENV: &str = "HOLO_AUTH_TOKEN";

/// Env var that redirects `holo`'s **model inference** to a self-hosted OpenAI-compatible
/// endpoint. When a local base URL is configured (alpha's local-only path, Project Aro PRD
/// P0-11), this daemon sets it -- alongside passing `holo serve --base-url <url>` -- so
/// inference goes to the local `llama-server` instead of H Company's hosted gateway.
///
/// This is deliberately **not** `HAI_BASE_URL`. Verified against the installed
/// `holo-desktop-cli` source (`~/.holo/tools/holo-desktop-cli/.../holo_desktop/`):
///
/// - `cli/agent_api.py` maps the `--base-url` CLI flag to `HAI_AGENT_RUNTIME_BASE_URL`
///   (`extra["HAI_AGENT_RUNTIME_BASE_URL"] = base_url`).
/// - `agent_client/launcher.py::runtime_child_env` propagates `HAI_AGENT_RUNTIME_BASE_URL` to the
///   runtime child **and removes `HAI_API_KEY`** from that child's env when it is set ("a custom
///   base URL points the runtime at a self-hosted endpoint; the portal `HAI_API_KEY` must not leak
///   to it"). That deletion is the concrete no-cloud enforcement.
/// - `agent_client/model_gateway.py` shows `HAI_BASE_URL` only overrides the *cloud entitlement
///   gateway region*, **not** the inference endpoint -- so `HAI_BASE_URL` is the wrong variable
///   for local inference and is never used here.
///
/// See [`crate::local_model`]'s module doc for the full citation chain.
pub const HAI_RUNTIME_BASE_URL_ENV: &str = crate::local_model::RUNTIME_BASE_URL_ENV;

/// Default A2A port per `serve.py`'s `A2A_DEFAULT_PORT`. The daemon does not have to use this
/// default -- `main.rs` picks its own port explicitly (env-overridable, defaulting to a
/// different value to avoid colliding with a `holo serve` an operator might run by hand
/// alongside this daemon) -- but it's recorded here since it's the value `holo serve` binds
/// to when `--port` is omitted, and is useful documentation/a fallback for any call site that
/// wants "the CLI's own default" specifically.
#[allow(dead_code)]
pub const HOLO_SERVE_DEFAULT_PORT: u16 = 18794;

/// How long to wait for `holo serve`'s `/health` to come up after spawn. `hai-agent-runtime`
/// itself may need to download on first run (`runtime_install.py`), so this is generous --
/// longer than the ~45s `SPAWN_TIMEOUT_S` the CLI's own inner spawn uses for the runtime binary,
/// to leave room for the outer `holo serve` process startup on top of that.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(90);
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(300);

/// A running `holo serve` child process plus everything needed to talk to it.
pub struct HoloServeProcess {
    child: Child,
    /// Not read internally today (`base_url` already embeds it) -- kept as a plain field for
    /// diagnostics/logging call sites that want the bare port number without re-parsing it out
    /// of `base_url`.
    #[allow(dead_code)]
    pub port: u16,
    pub base_url: String,
    pub auth_token: String,
    /// This process's claim on the single-instance guard. `Some` while this object is the
    /// tracked live child; taken (dropped -> released) by `shutdown()` and by
    /// [`Self::disarm_guard`] (the restart path's disarm-old-before-spawn-new step). A `None`
    /// claim means this object is a known-dead placeholder whose `Drop` must not (and cannot)
    /// touch the shared flag.
    guard: Option<GuardClaim>,
}

impl HoloServeProcess {
    /// Build the exact [`Command`] the daemon would spawn for `holo serve`, **without spawning
    /// it** and without generating a token (the token is generated per-spawn inside
    /// [`Self::spawn`]). This is the single source of truth for the `holo serve` invocation so the
    /// argument list and inference-redirect env can be inspected on their own -- see
    /// `examples/local_model_probe.rs`, which calls this to witness that `--base-url` is present
    /// and `HAI_AGENT_RUNTIME_BASE_URL` (not `HAI_BASE_URL`) is set for the local path.
    ///
    /// `local_base_url` is `Some(url)` when inference should go to a local OpenAI-compatible server
    /// (alpha's local `llama-server`); `None` leaves `holo serve` on its own configured (hosted)
    /// backend. When `Some`, this both appends `--base-url <url>` to the args **and** sets
    /// `HAI_AGENT_RUNTIME_BASE_URL=<url>` in the child env -- either alone suffices per
    /// `holo-desktop-cli`'s source, but setting both is belt-and-suspenders and makes the intent
    /// obvious in the argv. `auth_token` is the bearer token to export via `HOLO_AUTH_TOKEN`.
    pub fn build_command(
        holo_bin: &str,
        port: u16,
        local_base_url: Option<&str>,
        auth_token: &str,
    ) -> Command {
        let mut cmd = Command::new(holo_bin);
        cmd.arg("serve").arg("--port").arg(port.to_string());
        if let Some(url) = local_base_url {
            // `holo serve`'s `serve()` accepts `--base-url` as a real tyro CLI arg (cli/serve.py),
            // threaded to the agent runtime as HAI_AGENT_RUNTIME_BASE_URL.
            cmd.arg("--base-url").arg(url);
            // Belt-and-suspenders: also set the env var the flag maps to, and explicitly drop the
            // hosted key so it can never reach the self-hosted inference path (mirroring
            // launcher.py::runtime_child_env's own `env.pop("HAI_API_KEY")` on this branch). The
            // no-cloud guarantee (P0-11) does not depend on the child's own popping logic firing.
            cmd.env(HAI_RUNTIME_BASE_URL_ENV, url);
            cmd.env_remove("HAI_API_KEY");
        }
        cmd.env(HOLO_AUTH_TOKEN_ENV, auth_token);
        cmd
    }

    /// Spawn `holo serve --port <port>` as a managed subprocess, generating our own bearer
    /// token and exporting it via `HOLO_AUTH_TOKEN` so no stderr-scraping is needed. Waits for
    /// `/health` to report ok before returning.
    ///
    /// `holo_bin` is the path to (or bare name of) the `holo` CLI executable; resolved via
    /// `PATH` by `tokio::process::Command` when it's a bare name like `"holo"`. `local_base_url`
    /// points inference at a local OpenAI-compatible server when `Some` (alpha's local path);
    /// see [`Self::build_command`].
    pub async fn spawn(holo_bin: &str, port: u16, local_base_url: Option<&str>) -> Result<Self> {
        // Refuse to double-spawn: at most one `holo serve` child may be tracked as running by
        // this daemon at a time (see `HOLO_SERVE_RUNNING` / `GuardClaim` docs). The claim is an
        // owned token: any early `?`/`bail!` below drops it, releasing the guard automatically --
        // no manual store-on-error path to forget.
        let Some(guard) = GuardClaim::try_acquire() else {
            bail!(
                "holo serve is already running (tracked child process exists); refusing to spawn a second instance"
            );
        };

        let mut process = Self::spawn_inner(holo_bin, port, local_base_url).await?;
        process.guard = Some(guard);
        Ok(process)
    }

    async fn spawn_inner(holo_bin: &str, port: u16, local_base_url: Option<&str>) -> Result<Self> {
        let auth_token = generate_token();
        let base_url = format!("http://127.0.0.1:{port}");

        // Port preflight: if something else is already listening on the port, `holo serve` will
        // die on bind -- worse, the health probe below polls `http://127.0.0.1:{port}/...`,
        // which the SQUATTER answers, so without this check the daemon can conclude "healthy"
        // while its own child is already dead (witnessed live: a stale test stub on 8765 made
        // the daemon report a healthy bridge, then the health loop found the real child dead
        // and spun on restarts). Bind-and-release has a small TOCTOU window, which is fine:
        // this is a diagnosability preflight producing an actionable error for the overwhelmingly
        // common case, not the enforcement mechanism (bind failure inside holo serve remains the
        // authoritative failure).
        if let Err(err) = std::net::TcpListener::bind(("127.0.0.1", port)) {
            bail!(
                "port {port} is already in use by another process ({err}); `holo serve` cannot bind it. \
                 Find the squatter with `lsof -nP -iTCP:{port} -sTCP:LISTEN`, kill it, or pick a \
                 different port via HOLOIROH_HOLO_PORT"
            );
        }

        // Build the exact command via the shared builder (also used by the verification example),
        // then add only the stdio/kill-on-drop settings that don't affect the argv/env contract.
        let mut cmd = Self::build_command(holo_bin, port, local_base_url, &auth_token);
        cmd
            // Inherit the parent's env otherwise (HAI_API_KEY when NOT local, etc. from
            // mac-daemon/.env / the launching shell) so `holo serve`'s own settings loader
            // (settings.py) sees the same auth/gateway config the operator configured. When
            // `local_base_url` is Some, `build_command` has already removed HAI_API_KEY.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        if let Some(url) = local_base_url {
            tracing::info!(local_base_url = %url, "holo serve will use LOCAL inference (no cloud path)");
        }

        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn `{holo_bin} serve --port {port}`"))?;

        // Drain stdout/stderr into the tracing log instead of letting the pipes fill and block
        // the child once the OS pipe buffer is exhausted (holo serve writes its startup banner,
        // including the token line if HOLO_AUTH_TOKEN was NOT honored for some reason, to
        // stderr via `rich.console.Console(stderr=True)`).
        if let Some(stdout) = child.stdout.take() {
            spawn_log_drain(stdout, "holo serve", tracing::Level::DEBUG);
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_log_drain(stderr, "holo serve", tracing::Level::INFO);
        }

        wait_for_health(&base_url, &mut child).await?;

        tracing::info!(port, base_url = %base_url, "holo serve is healthy");

        Ok(Self {
            child,
            port,
            base_url,
            auth_token,
            // The single-instance claim is attached by `spawn` (the only caller) after this
            // returns -- `spawn_inner` itself never owns it, so its `?` error paths can't
            // affect the guard.
            guard: None,
        })
    }

    /// Drop this process's claim on the single-instance guard, marking it a known-dead
    /// placeholder. The restart path (`HoloBridge::restart_process`) calls this on the
    /// dead-child process BEFORE spawning the replacement -- otherwise the replacement's
    /// `GuardClaim::try_acquire` would fail against the dead process's still-held claim (the
    /// deterministic "failed to respawn holo serve" loop this design fixes). After disarming,
    /// this object's `Drop` still SIGTERMs the (already-exited) child as a harmless safety net
    /// but cannot touch the shared flag.
    pub fn disarm_guard(&mut self) {
        // Dropping the claim IS the release (GuardClaim::drop stores false).
        self.guard.take();
    }

    /// Build an [`A2aClient`] bound to this running server.
    pub fn client(&self) -> A2aClient {
        A2aClient::new(self.base_url.clone(), self.auth_token.clone())
    }

    /// PID of the `holo serve` process, for diagnostics.
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// Non-blocking liveness check: `Ok(None)` if still running, `Ok(Some(status))` if it has
    /// already exited (crashed or was killed outside this daemon's own shutdown path), `Err` on
    /// an OS-level error reaping the process. Thin wrapper over `tokio::process::Child::try_wait`
    /// -- see `holo_bridge::health`, the only caller.
    pub fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.child.try_wait()
    }

    /// Terminate `holo serve`. Sends SIGTERM first (mirrors the CLI's own `Ctrl+C to stop`
    /// story -- `holo serve` has no documented graceful-shutdown RPC of its own, it relies on
    /// process signals / uvicorn's own signal handling), then force-kills if it doesn't exit
    /// promptly. This is the primary (async, awaitable) shutdown path; [`Drop`] below is a
    /// best-effort synchronous safety net for the case where this was never called (early
    /// return, panic unwind), not a replacement for it -- calling this explicitly is always
    /// preferred since it can actually wait for exit.
    pub async fn shutdown(mut self) -> Result<()> {
        let result = self.terminate_and_wait().await;
        // Release this process's claim regardless of outcome, so a later `spawn()` isn't
        // permanently refused because this one failed to confirm exit. Dropping the claim is
        // the release; if this object never held one (already disarmed by the restart path),
        // this is a no-op and cannot clobber a newer process's claim.
        self.guard.take();
        result
    }

    /// Shared SIGTERM-then-wait-then-SIGKILL logic used by both [`Self::shutdown`] and, as a
    /// synchronous last resort, [`Drop`].
    async fn terminate_and_wait(&mut self) -> Result<()> {
        #[cfg(unix)]
        {
            if let Some(pid) = self.child.id() {
                // SAFETY: libc::kill with a valid pid and SIGTERM is a well-defined syscall;
                // failure (e.g. ESRCH if it already exited) is non-fatal here, we just fall
                // through to the timeout+kill below.
                unsafe {
                    libc::kill(pid as libc::pid_t, libc::SIGTERM);
                }
            }
        }
        let graceful = tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await;
        match graceful {
            Ok(Ok(status)) => {
                tracing::info!(?status, "holo serve exited after SIGTERM");
                Ok(())
            }
            _ => {
                tracing::warn!("holo serve did not exit within 5s of SIGTERM; killing");
                self.child.kill().await.context("failed to kill holo serve")?;
                Ok(())
            }
        }
    }
}

impl Drop for HoloServeProcess {
    /// Best-effort safety net for the case where [`Self::shutdown`] was never called (e.g. the
    /// daemon panicked, or a future call site drops a `HoloServeProcess` without awaiting
    /// shutdown). `Drop` cannot be `async`, so this cannot wait for graceful exit the way
    /// `shutdown()` does -- it best-effort SIGTERMs the child synchronously (same signal
    /// `shutdown()` sends first) and then relies on the `Command`'s own `kill_on_drop(true)`
    /// (set in `spawn_inner`) as the final backstop: when `self.child` (a `tokio::process::Child`)
    /// is dropped right after this, `kill_on_drop` SIGKILLs it if it's still alive. The
    /// single-instance guard releases via the owned `GuardClaim` field's own `Drop` (running as
    /// part of this object's field drops, after this body) -- and ONLY if this object still
    /// holds its claim: a disarmed process (see [`HoloServeProcess::disarm_guard`], the restart
    /// path) has no claim, so dropping it cannot release the flag its replacement now holds.
    /// The earlier unconditional `store(false)` here was exactly that bug.
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            if let Some(pid) = self.child.id() {
                tracing::warn!(pid, "HoloServeProcess dropped without shutdown(); sending SIGTERM as a safety net (kill_on_drop will SIGKILL if this doesn't land in time)");
                // SAFETY: same as in `terminate_and_wait` -- valid pid, well-defined syscall,
                // ESRCH-on-already-exited is a harmless no-op.
                unsafe {
                    libc::kill(pid as libc::pid_t, libc::SIGTERM);
                }
            }
        }
        // No synchronous wait here (Drop can't await); `kill_on_drop(true)` on the underlying
        // `Command` (see spawn_inner) guarantees the OS process doesn't outlive this `Child`
        // handle even if the SIGTERM above didn't have time to take effect.
    }
}

fn generate_token() -> String {
    use uuid::Uuid;
    // Two v4 UUIDs concatenated (no hyphens) gives 256 bits of randomness, comfortably matching
    // the entropy of the CLI's own `secrets.token_urlsafe(32)` generated tokens. This is our own
    // token (see module doc) -- its format has no contract with the CLI beyond "opaque bearer
    // string", so any sufficiently random string is fine.
    format!(
        "{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    )
}

async fn wait_for_health(base_url: &str, child: &mut Child) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .context("failed to build health-check HTTP client")?;
    let deadline = Instant::now() + HEALTH_TIMEOUT;
    let health_url = format!("{base_url}/health");

    loop {
        if let Some(status) = child.try_wait().context("failed to poll holo serve child status")? {
            bail!("holo serve exited during startup with status {status}");
        }

        match client.get(&health_url).send().await {
            Ok(resp) if resp.status().is_success() => return Ok(()),
            _ => {}
        }

        if Instant::now() >= deadline {
            let _ = child.kill().await;
            bail!(
                "holo serve did not become healthy within {:?} (GET {} never returned 2xx)",
                HEALTH_TIMEOUT,
                health_url
            );
        }
        sleep(HEALTH_POLL_INTERVAL).await;
    }
}

/// Pipes a child's stdout/stderr into `tracing` line-by-line so output isn't lost and the pipe
/// never backs up.
fn spawn_log_drain<R>(reader: R, label: &'static str, level: tracing::Level)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => match level {
                    tracing::Level::DEBUG => tracing::debug!(target: "holo_bridge::child", "{label}: {line}"),
                    _ => tracing::info!(target: "holo_bridge::child", "{label}: {line}"),
                },
                Ok(None) => break,
                Err(err) => {
                    tracing::warn!(target: "holo_bridge::child", "{label}: log drain error: {err}");
                    break;
                }
            }
        }
    });
}
