//! Spawns and supervises `holo serve` as a managed child process. It supplies the bearer token
//! and the bridge-owned inherited A2A listener.
//!
//! ## Source grounding
//!
//! Verified directly against the installed `holo-desktop-cli==0.0.2` source
//! (`holo_desktop/cli/serve.py`) and its installed `uvicorn==0.51.0`:
//!
//! - `holo serve` serves `127.0.0.1:<port>` (default port 18794, `A2A_DEFAULT_PORT`).
//!   Version 0.0.2 performs a plain preliminary bind probe before `uvicorn.run`. That probe is
//!   the EADDRINUSE restart bug. Uvicorn itself supports `fd=` and duplicates the inherited
//!   listener with `socket.fromfd`; it is not the source of the SO_REUSEADDR mismatch.
//! - Every route except `/health` requires `Authorization: Bearer <token>`
//!   (`BearerAuthMiddleware` in `serve.py`).
//! - The token comes from the `HOLO_AUTH_TOKEN` env var if set (`ServeSettings.auth_token`
//!   in `settings.py`). Otherwise, `serve()` generates one with
//!   `secrets.token_urlsafe(32)`. It **only ever surfaces it by printing it to stderr**:
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
//! - Health check: `GET /health` (no auth) returns
//!   `{"service": "holo-desktop", "status": "ok", "version": "<semver>"}`. This happens once
//!   `HoloExecutor.startup()` spawns or attaches the underlying `hai-agent-runtime` binary, and
//!   the Starlette app finishes its lifespan startup. The Rust parent is already listening while
//!   the child starts. Connections can therefore wait in the kernel accept queue. Each health
//!   request is bounded to two seconds and total startup is bounded to 90 seconds.

use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::time::{Instant, sleep};

use crate::holo_bridge::a2a_client::A2aClient;
use crate::holo_bridge::listener::{HoloA2aListener, inherited_holo_command};

/// This process-wide guard ensures at most one `holo serve` child is ever tracked as running by
/// this daemon at a time. [`HoloServeProcess::spawn`] refuses to spawn a second child while this
/// is `true`. An owned [`GuardClaim`] token holds this flag, rather than raw stores. So release
/// ties to ownership: only the object that acquired the claim can release it. It does so exactly
/// once. This exists because the earlier raw-`store(false)` design had two real bugs. Live use
/// witnessed both: via `holo_bridge::health`'s "restart failed: failed to respawn holo serve"
/// loop, and via `examples/serve_guard_probe.rs`:
///
/// 1. **Restart could never succeed.** `HoloBridge::restart_process` spawned the NEW child while
///    the old (dead-child) `HoloServeProcess` still held the guard. `compare_exchange` failed
///    every time, deterministically. So the health loop errored every tick forever.
/// 2. **The old process's `Drop` released the new process's guard.** `Drop` did an unconditional
///    `store(false)`. After a (hypothetically successful) restart replaced the old object, the
///    old `Drop` would clear the claim the NEW child just acquired. This would re-open the
///    double-spawn hole the guard exists to close.
///
/// With [`GuardClaim`], the restart path disarms the old process (drops its claim) before
/// spawning the replacement. A claim-less process's `Drop` cannot touch the flag at all.
static HOLO_SERVE_RUNNING: AtomicBool = AtomicBool::new(false);

/// Owned claim on [`HOLO_SERVE_RUNNING`]. Acquiring the claim ([`GuardClaim::try_acquire`])
/// atomically flips the flag `false -> true`. Dropping the claim releases it (`-> false`). This
/// happens exactly once, and only for the claim that actually holds it. Zero-sized -- the token
/// IS the ownership.
///
/// This type is public (not just `pub(crate)`), so `examples/serve_guard_probe.rs` can witness
/// two things against the REAL static, per this repo's probe-witness rule:
/// - The acquire/second-acquire-fails/release/re-acquire lifecycle.
/// - The restart-ordering: disarm-old-then-acquire-new leaves the flag held by the new claim,
///   and dropping the disarmed old is a no-op.
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
/// P0-11), this daemon sets it, alongside passing `holo serve --base-url <url>`. So inference
/// goes to the local `llama-server` instead of H Company's hosted gateway.
///
/// This is deliberately **not** `HAI_BASE_URL`. Verified against the installed
/// `holo-desktop-cli` source (`~/.holo/tools/holo-desktop-cli/.../holo_desktop/`):
///
/// - `cli/agent_api.py` maps the `--base-url` CLI flag to `HAI_AGENT_RUNTIME_BASE_URL`
///   (`extra["HAI_AGENT_RUNTIME_BASE_URL"] = base_url`).
/// - `agent_client/launcher.py::runtime_child_env` propagates `HAI_AGENT_RUNTIME_BASE_URL` to
///   the runtime child. It also removes `HAI_API_KEY` from that child's env when it is set ("a
///   custom base URL points the runtime at a self-hosted endpoint; the portal `HAI_API_KEY`
///   must not leak to it"). That deletion is the concrete no-cloud enforcement.
/// - `agent_client/model_gateway.py` shows that `HAI_BASE_URL` only overrides the *gateway
///   region for cloud entitlement*, not the inference endpoint. So `HAI_BASE_URL` is the wrong
///   variable for local inference. This module never uses it here.
///
/// See [`crate::local_model`]'s module doc for the full citation chain.
pub const HAI_RUNTIME_BASE_URL_ENV: &str = crate::local_model::RUNTIME_BASE_URL_ENV;

/// Env var naming the port `holo serve`'s underlying `hai-agent-runtime` child binds
/// (`settings.py`: `runtime.port`, read via `HAI_AGENT_RUNTIME_PORT`; default 18795).
///
/// The daemon ALWAYS sets this to its own private port ([`daemon_runtime_port`]). The launcher
/// (`launcher.py::ensure_running`) ATTACHES to any healthy runtime already on the port, instead
/// of spawning. Spawn-time knobs -- `--base-url`, `--model` -- "only reach a freshly spawned
/// process" (SpawnConfig's own doc).
///
/// Without a private port, this fails silently in two ways. A runtime left over from the
/// operator's own `holo run` on the default port would attach silently. Every
/// inference-backend setting this daemon passes would then be IGNORED. This would make the
/// rate-limit fallback (and local no-cloud mode) silent no-ops.
pub const HAI_RUNTIME_PORT_ENV: &str = "HAI_AGENT_RUNTIME_PORT";

/// The daemon's private `hai-agent-runtime` port. This is env-overridable via
/// `HOLOIROH_AGENT_RUNTIME_PORT`. It defaults to 18899, distinct from the runtime's own 18795
/// default, so operator-run `holo` CLI sessions and this daemon never share a runtime.
pub fn daemon_runtime_port() -> u16 {
    std::env::var("HOLOIROH_AGENT_RUNTIME_PORT")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(18899)
}

/// Best-effort teardown of a stale `hai-agent-runtime` on the daemon's private port before
/// (re)spawning `holo serve`. This function is needed because a SIGKILLed `holo serve` (the
/// escalation path in [`HoloServeProcess::shutdown`]) can orphan the runtime child it spawned.
/// The NEXT `holo serve` would then attach to that still-healthy runtime and inherit its OLD
/// inference-backend config (see [`HAI_RUNTIME_PORT_ENV`] -- attach ignores spawn knobs).
/// The pid comes from the launcher's own `~/.holo/agent-pid-<port>` file. The port is
/// daemon-private by construction, so any process recorded there is ours to reap.
fn reap_stale_runtime(runtime_port: u16) {
    let pid_path = dirs_home().join(format!(".holo/agent-pid-{runtime_port}"));
    let Ok(contents) = std::fs::read_to_string(&pid_path) else {
        return;
    };
    let Ok(pid) = contents.trim().parse::<i32>() else {
        return;
    };
    #[cfg(unix)]
    unsafe {
        // SAFETY: kill(pid, 0) only probes liveness; SIGTERM/SIGKILL on a pid recorded in
        // the daemon-private port's pid file targets a process this daemon's own earlier
        // `holo serve` spawned.
        if libc::kill(pid, 0) != 0 {
            return; // already gone; the launcher will overwrite the stale pid file
        }
        tracing::warn!(
            pid,
            runtime_port,
            "stale hai-agent-runtime found on the daemon's private port; terminating so the fresh spawn's backend config applies"
        );
        libc::kill(pid, libc::SIGTERM);
        for _ in 0..30 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if libc::kill(pid, 0) != 0 {
                return;
            }
        }
        libc::kill(pid, libc::SIGKILL);
    }
}

fn dirs_home() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/"))
}

/// Default A2A port per `serve.py`'s `A2A_DEFAULT_PORT`. The daemon does not have to use this
/// default. `main.rs` picks its own port explicitly. That port is env-overridable, and it
/// defaults to a different value, to avoid colliding with a `holo serve` an operator might run
/// by hand alongside this daemon.
///
/// This constant is recorded here for two reasons. First, it is the value `holo serve` binds to
/// when `--port` is omitted. Second, it is useful documentation, and a fallback, for any call
/// site that wants "the CLI's own default" specifically.
#[allow(dead_code)]
pub const HOLO_SERVE_DEFAULT_PORT: u16 = 18794;

/// How long to wait for `holo serve`'s `/health` to come up after spawn. `hai-agent-runtime`
/// itself may need to download on first run (`runtime_install.py`), so this is generous. It is
/// longer than the ~45s `SPAWN_TIMEOUT_S` the CLI's own inner spawn uses for the runtime
/// binary. This leaves room for the outer `holo serve` process startup, on top of that.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(90);
const HEALTH_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(300);

#[derive(serde::Deserialize)]
struct HealthResponse {
    service: String,
    status: String,
    version: String,
}

/// A running `holo serve` child process plus everything needed to talk to it.
pub struct HoloServeProcess {
    child: Child,
    _standalone_listener: Option<HoloA2aListener>,
    /// This field is not read internally today (`base_url` already embeds it). It stays as a
    /// plain field for diagnostics/logging call sites that want the bare port number, without
    /// re-parsing it out of `base_url`.
    #[allow(dead_code)]
    pub port: u16,
    pub base_url: String,
    pub auth_token: String,
    /// This process's claim on the single-instance guard. `Some` while this object is the
    /// tracked live child. `shutdown()` and [`Self::disarm_guard`] take it -- dropping it
    /// releases it. [`Self::disarm_guard`] is the restart path's disarm-old-before-spawn-new
    /// step. A `None` claim means this object is a known-dead placeholder whose `Drop` must
    /// not (and cannot) touch the shared flag.
    guard: Option<GuardClaim>,
}

impl HoloServeProcess {
    /// Build the equivalent upstream `holo serve` command without spawning it. This inspection
    /// helper preserves the existing example-probe API. Production child creation runs the
    /// repository shim through [`inherited_holo_command`], while both paths share
    /// [`Self::configure_serve_command`] for every serve argument and inference environment value.
    ///
    /// `local_base_url` is `Some(url)` when inference should go to a local OpenAI-compatible server
    /// (alpha's local `llama-server`, or the loopback tinfoil fallback proxy -- see
    /// `crate::tinfoil_proxy`); `None` leaves `holo serve` on its own configured (hosted)
    /// backend. When `Some`, this both appends `--base-url <url>` to the args **and** sets
    /// `HAI_AGENT_RUNTIME_BASE_URL=<url>` in the child env -- either alone suffices per
    /// `holo-desktop-cli`'s source, but setting both is belt-and-suspenders and makes the intent
    /// obvious in the argv.
    ///
    /// `model_override` names the model the agent runtime should request from that endpoint
    /// (`holo serve --model`, threaded to the runtime as `HAI_AGENT_RUNTIME_MODEL` -- same
    /// flag-plus-env pattern as the base URL). `None` keeps the runtime's own default. Only
    /// meaningful alongside a custom base URL: the endpoint routes by model name (the local
    /// llama-server ignores it; tinfoil requires `kimi-k2-6`).
    ///
    /// `auth_token` is the bearer token to export via `HOLO_AUTH_TOKEN`.
    pub fn build_command(
        holo_bin: &str,
        port: u16,
        local_base_url: Option<&str>,
        model_override: Option<&str>,
        auth_token: &str,
    ) -> Command {
        let mut cmd = Command::new(holo_bin);
        cmd.arg("serve");
        Self::configure_serve_command(&mut cmd, port, local_base_url, model_override, auth_token);
        cmd
    }

    fn configure_serve_command(
        cmd: &mut Command,
        port: u16,
        local_base_url: Option<&str>,
        model_override: Option<&str>,
        auth_token: &str,
    ) {
        cmd.arg("--port").arg(port.to_string());
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
        if let Some(model) = model_override {
            // `--model` maps to HAI_AGENT_RUNTIME_MODEL in cli/serve.py -> SpawnConfig.model ->
            // launcher.py::_spawn, identical shape to the base-url plumbing above.
            cmd.arg("--model").arg(model);
            cmd.env("HAI_AGENT_RUNTIME_MODEL", model);
        }
        // Always pin the runtime child to the daemon's private port -- see
        // HAI_RUNTIME_PORT_ENV's doc for why sharing the default port with operator-run
        // `holo` sessions silently discards every backend setting above.
        cmd.env(HAI_RUNTIME_PORT_ENV, daemon_runtime_port().to_string());
        cmd.env(HOLO_AUTH_TOKEN_ENV, auth_token);
    }

    /// Spawn `holo serve --port <port>` as a managed subprocess, generating our own bearer
    /// token and exporting it via `HOLO_AUTH_TOKEN` so no stderr-scraping is needed. Waits for
    /// `/health` to report ok before returning.
    ///
    /// `holo_bin` is the path to (or bare name of) the `holo` CLI launcher. The inherited-listener
    /// helper resolves its Python shebang so the embedded compatibility shim runs inside the same
    /// installed environment. `local_base_url` points inference at a local OpenAI-compatible server
    /// when `Some` (alpha's local path); see [`Self::build_command`].
    /// Standalone compatibility entry point for examples that manage one process directly. It
    /// owns its listener in the returned process. [`crate::holo_bridge::HoloBridge`] instead calls
    /// [`Self::spawn_on_listener`] so one bridge-owned listener survives child replacement.
    pub async fn spawn(
        holo_bin: &str,
        port: u16,
        local_base_url: Option<&str>,
        model_override: Option<&str>,
    ) -> Result<Self> {
        let listener = HoloA2aListener::bind(port)?;
        let mut process =
            Self::spawn_on_listener(holo_bin, &listener, local_base_url, model_override).await?;
        process._standalone_listener = Some(listener);
        Ok(process)
    }

    /// Spawn one managed child against a listener owned by its caller.
    pub async fn spawn_on_listener(
        holo_bin: &str,
        listener: &HoloA2aListener,
        local_base_url: Option<&str>,
        model_override: Option<&str>,
    ) -> Result<Self> {
        let Some(guard) = GuardClaim::try_acquire() else {
            bail!(
                "holo serve is already running (tracked child process exists); refusing to spawn a second instance"
            );
        };

        let mut process =
            Self::spawn_inner(holo_bin, listener, local_base_url, model_override).await?;
        process.guard = Some(guard);
        Ok(process)
    }

    async fn spawn_inner(
        holo_bin: &str,
        listener: &HoloA2aListener,
        local_base_url: Option<&str>,
        model_override: Option<&str>,
    ) -> Result<Self> {
        let auth_token = generate_token();
        let port = listener.port();
        let base_url = format!("http://127.0.0.1:{port}");
        Self::spawn_attempt(
            holo_bin,
            listener,
            local_base_url,
            model_override,
            &auth_token,
            &base_url,
        )
        .await
    }

    /// Spawns one child against the listener that the bridge already owns, then waits for the
    /// existing startup health gate. Holo 0.0.2's preliminary bind probe is bypassed by the
    /// repository compatibility shim; Uvicorn receives the inherited descriptor directly.
    async fn spawn_attempt(
        holo_bin: &str,
        listener: &HoloA2aListener,
        local_base_url: Option<&str>,
        model_override: Option<&str>,
        auth_token: &str,
        base_url: &str,
    ) -> Result<Self> {
        let port = listener.port();
        // A stale runtime on the daemon's private port would be silently ATTACHED (backend
        // config discarded) -- reap it so this spawn's config actually applies.
        reap_stale_runtime(daemon_runtime_port());

        let mut cmd = inherited_holo_command(holo_bin, listener)?;
        Self::configure_serve_command(&mut cmd, port, local_base_url, model_override, auth_token);
        cmd
            // Inherit the parent's env otherwise (HAI_API_KEY when NOT local, etc. from
            // mac-daemon/.env / the launching shell) so `holo serve`'s own settings loader
            // (settings.py) sees the same auth/gateway config the operator configured. When
            // `local_base_url` is `Some`, `configure_serve_command` has already removed HAI_API_KEY.
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

        wait_for_health(base_url, &mut child).await?;

        tracing::info!(port, base_url = %base_url, "holo serve is healthy");

        Ok(Self {
            child,
            _standalone_listener: None,
            port,
            base_url: base_url.to_string(),
            auth_token: auth_token.to_string(),
            // The single-instance claim is attached by `spawn_on_listener` after this returns.
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
    /// Terminate the (possibly still-LIVE) child in place, releasing its single-instance
    /// guard claim, without consuming `self`. This is the backend-switch primitive
    /// (`HoloBridge::switch_backend`): unlike the crash-restart path -- where the child is
    /// already dead and `disarm_guard` alone suffices -- switching inference backends must
    /// first stop a healthy running `holo serve` so only its replacement accepts from the
    /// inherited listener and acquires the guard. The dead process object stays in its slot
    /// afterwards; the caller swaps in the replacement.
    pub async fn terminate_in_place(&mut self) -> Result<()> {
        self.terminate_and_wait().await?;
        self.guard.take();
        Ok(())
    }

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
                self.child
                    .kill()
                    .await
                    .context("failed to kill holo serve")?;
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
                tracing::warn!(
                    pid,
                    "HoloServeProcess dropped without shutdown(); sending SIGTERM as a safety net (kill_on_drop will SIGKILL if this doesn't land in time)"
                );
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
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

async fn wait_for_health(base_url: &str, child: &mut Child) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(HEALTH_REQUEST_TIMEOUT)
        .build()
        .context("failed to build health-check HTTP client")?;
    let deadline = Instant::now() + HEALTH_TIMEOUT;
    let health_url = format!("{base_url}/health");

    loop {
        if let Some(status) = child
            .try_wait()
            .context("failed to poll holo serve child status")?
        {
            bail!("holo serve exited during startup with status {status}");
        }

        if let Ok(resp) = client.get(&health_url).send().await {
            if resp.status().is_success() {
                if let Ok(health) = resp.json::<HealthResponse>().await {
                    if health.service == "holo-desktop"
                        && health.status == "ok"
                        && !health.version.trim().is_empty()
                    {
                        return Ok(());
                    }
                }
            }
        }

        if Instant::now() >= deadline {
            let _ = child.kill().await;
            let _ = child.wait().await;
            bail!(
                "holo serve did not become healthy within {:?} (GET {} never returned the expected JSON within {:?} per request)",
                HEALTH_TIMEOUT,
                health_url,
                HEALTH_REQUEST_TIMEOUT
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
                    tracing::Level::DEBUG => {
                        tracing::debug!(target: "holo_bridge::child", "{label}: {line}")
                    }
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
