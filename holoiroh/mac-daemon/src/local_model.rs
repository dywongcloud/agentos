//! Manages a local [`llama.cpp`](https://github.com/ggml-org/llama.cpp) `llama-server`
//! subprocess serving the on-device Holo3.1 vision model, so the daemon's alpha inference path
//! is **fully local** -- no H Company hosted API, no cloud egress at all (Project Aro PRD row
//! P0-11, "Aro Private mode": the alpha binary must contain no cloud inference code path).
//!
//! ## What this serves, and why local
//!
//! `holo serve` (see [`crate::holo_bridge::process`]) fronts the closed-source
//! `hai-agent-runtime`, which talks to a model over an **OpenAI-compatible** `chat.completions`
//! endpoint. By default that endpoint is H Company's hosted gateway. Pointing it instead at a
//! `llama-server` running on `127.0.0.1` makes every inference call stay on this machine. That is
//! exactly what [`BENCHMARKS.md`](../../BENCHMARKS.md) measured: `llama-server` on `127.0.0.1:8080`
//! serving `Hcompany/Holo-3.1-35B-A3B-GGUF:Q4_K_M`, real vision `chat.completions` requests, a
//! measured **8.3 s/step at 720p** on an Apple M3 Pro / 36 GB Mac.
//!
//! ## The command this builds
//!
//! ```text
//! llama-server \
//!   -hf Hcompany/Holo-3.1-35B-A3B-GGUF:Q4_K_M \
//!   --host 127.0.0.1 \
//!   --port <N>
//! ```
//!
//! - **`-hf <repo>:<quant>`** resolves the model from the Hugging Face cache (already downloaded
//!   to `~/.cache/huggingface/hub/models--Hcompany--Holo-3.1-35B-A3B-GGUF` on this machine). The
//!   repo ships **both** `q4_k_m.gguf` and a vision projector `mmproj.f16.gguf`; `llama-server`'s
//!   `-hf` flag **auto-downloads/loads the mmproj when the repo has one** (its own `--help`:
//!   "mmproj is also downloaded automatically if available. to disable, add `--no-mmproj`"). The
//!   projector is load-bearing here -- Holo3.1 is a *vision* model and the whole point is sending
//!   desktop screenshots to it -- so this deliberately does **not** pass `--no-mmproj`.
//! - **`--host 127.0.0.1`** binds loopback only. `llama-server`'s default host is already
//!   `127.0.0.1`, but this passes it explicitly and never accepts a caller-supplied host, so the
//!   local inference endpoint is structurally unreachable off-box (defense in depth for the
//!   no-cloud / no-external-exposure posture; see [`LocalModelConfig::command_args`]).
//! - **`--port <N>`** is the OpenAI-compatible HTTP port. The daemon defaults this to `8080`
//!   (matching `BENCHMARKS.md`), env-overridable via `HOLOIROH_LOCAL_MODEL_PORT`. It must differ
//!   from the `holo serve` A2A port (`HOLOIROH_HOLO_PORT`, default `8765`): these are two distinct
//!   listeners.
//!
//! The base URL a co-process points at is **`http://127.0.0.1:<N>/v1`** -- `llama-server` serves
//! the OpenAI-compatible routes (`/v1/chat/completions`, `/v1/models`) under the `/v1` prefix, and
//! its plain-JSON health/readiness route is `/health` at the root.
//!
//! ## How `holo serve` is pointed here (the env-var that actually matters)
//!
//! This is the load-bearing correctness detail, verified directly against the installed
//! `holo-desktop-cli` source (`~/.holo/tools/holo-desktop-cli/.../holo_desktop/`, the same commit
//! this daemon's `holo_bridge` is grounded on):
//!
//! - `holo`'s **`--base-url` CLI flag maps to the `HAI_AGENT_RUNTIME_BASE_URL` env var**
//!   (`cli/agent_api.py`: `extra["HAI_AGENT_RUNTIME_BASE_URL"] = base_url`), which redirects the
//!   **model-inference** endpoint the agent runtime calls.
//! - When that runtime base URL is set, `agent_client/launcher.py::runtime_child_env` **removes
//!   `HAI_API_KEY` from the runtime child's environment** ("a custom base URL points the runtime
//!   at a self-hosted endpoint; the portal `HAI_API_KEY` must not leak to it") and skips
//!   `apply_hosted_gateway_default`. That deletion is the concrete no-cloud enforcement: with a
//!   local base URL set, the hosted key never reaches the inference path.
//! - `cli/bootstrap.py::require_api_key` early-returns (skips `holo login`) when a base URL is
//!   supplied -- "Skip with `--base-url` for a local model."
//!
//! **`HAI_BASE_URL` is a different variable and must not be used for this.** Per
//! `agent_client/model_gateway.py`, `HAI_BASE_URL` only overrides the *entitlement-probe gateway
//! region* (a cloud control-plane URL), not the inference endpoint. Setting `HAI_BASE_URL=http://
//! localhost:...` would leave inference pointed at the cloud while breaking the entitlement probe
//! -- the opposite of the intent. The daemon therefore sets **`HAI_AGENT_RUNTIME_BASE_URL`** (and,
//! belt-and-suspenders, also passes `holo serve --base-url <url>`, which `cli/serve.py`'s `serve()`
//! accepts as a real `tyro` CLI argument and threads to the same setting). See
//! [`RUNTIME_BASE_URL_ENV`] and [`crate::holo_bridge::process`].
//!
//! ## What is and is not verified in-repo
//!
//! The **command construction and env wiring** are real and are witnessed by
//! `cargo run --example local_model_probe`, which builds the exact `Command`s this module and
//! `holo_bridge::process` produce and prints their program/args/env **without spawning the
//! model**. A **full live model-serving run is intentionally not performed in that verification**:
//! the GGUF is ~21 GB and takes minutes plus large RAM to load, so re-running it every build would
//! be wasteful and slow. The real end-to-end latency of actually serving this model locally is
//! measured separately and honestly in [`BENCHMARKS.md`](../../BENCHMARKS.md) (8.3 s/step @ 720p on
//! this hardware), not re-derived here.

use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::time::{Instant, sleep};

/// The Hugging Face repo + quant the alpha build serves, in `llama-server -hf` syntax
/// (`<user>/<model>:<quant>`). Already present in this machine's HF cache. `-hf` resolves the
/// accompanying `mmproj.f16.gguf` vision projector automatically (see module doc).
pub const DEFAULT_MODEL_HF_REPO: &str = "Hcompany/Holo-3.1-35B-A3B-GGUF:Q4_K_M";

/// Default `llama-server` executable name. Resolved via `PATH` by [`Command`] when left as a bare
/// name (Homebrew installs it at `/opt/homebrew/bin/llama-server`, which is on a standard `PATH`).
pub const DEFAULT_LLAMA_SERVER_BIN: &str = "llama-server";

/// Default OpenAI-compatible port `llama-server` listens on. Matches the port `BENCHMARKS.md`'s
/// real run used (`127.0.0.1:8080`), and `llama-server`'s own default. Must differ from the
/// `holo serve` A2A port (`main.rs`'s `holo_serve_port()`, default `8765`).
pub const DEFAULT_LOCAL_MODEL_PORT: u16 = 8080;

/// The **only** host `llama-server` is ever bound to. Loopback, never caller-overridable to
/// `0.0.0.0` or a routable address -- the local inference endpoint must not be reachable off-box.
pub const LOOPBACK_HOST: &str = "127.0.0.1";

/// The env var `holo-desktop-cli` reads to redirect **model inference** to a self-hosted
/// OpenAI-compatible endpoint. This -- **not** `HAI_BASE_URL` -- is what makes the alpha path
/// local/no-cloud. See the module doc's "How `holo serve` is pointed here" section for the source
/// citations; also consumed by [`crate::holo_bridge::process`] when spawning `holo serve`.
pub const RUNTIME_BASE_URL_ENV: &str = "HAI_AGENT_RUNTIME_BASE_URL";

/// How long to wait for `llama-server`'s `/health` to report ready after spawn. **Deliberately
/// large**: loading the ~21 GB GGUF (plus the vision projector) into memory and warming Metal can
/// take minutes on first load, far longer than `holo serve`'s own startup. The daemon does not
/// spawn this on every build -- only at real runtime -- so a generous ceiling is appropriate.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(600);
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Process-wide guard: at most one `llama-server` child tracked as running by this daemon at a
/// time. Mirrors `holo_bridge::process`'s `HOLO_SERVE_RUNNING` -- [`LocalModelServer::spawn`]
/// refuses to start a second child while this is `true`, and every exit path clears it.
static LOCAL_MODEL_RUNNING: AtomicBool = AtomicBool::new(false);

/// Everything needed to build the `llama-server` command and derive the base URL a co-process
/// (the daemon-spawned `holo serve`) points its inference at. Deliberately split from
/// [`LocalModelServer`] (the *running* process) so the exact command and URL can be constructed
/// and inspected **without spawning the model** -- see [`Self::command`] / [`Self::base_url`] and
/// `examples/local_model_probe.rs`.
#[derive(Debug, Clone)]
pub struct LocalModelConfig {
    /// `llama-server` executable (bare name resolved via `PATH`, or an absolute path).
    pub llama_server_bin: String,
    /// Model in `-hf` syntax (`<user>/<model>:<quant>`).
    pub model_hf_repo: String,
    /// Loopback port for the OpenAI-compatible HTTP server.
    pub port: u16,
}

impl Default for LocalModelConfig {
    fn default() -> Self {
        Self {
            llama_server_bin: DEFAULT_LLAMA_SERVER_BIN.to_string(),
            model_hf_repo: DEFAULT_MODEL_HF_REPO.to_string(),
            port: DEFAULT_LOCAL_MODEL_PORT,
        }
    }
}

impl LocalModelConfig {
    /// Build a config from the process environment, applying defaults for anything unset:
    /// `HOLOIROH_LLAMA_BIN`, `HOLOIROH_LOCAL_MODEL_HF_REPO`, `HOLOIROH_LOCAL_MODEL_PORT`. Never
    /// reads a host override -- the bind host is always [`LOOPBACK_HOST`].
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if let Ok(bin) = std::env::var("HOLOIROH_LLAMA_BIN") {
            if !bin.trim().is_empty() {
                cfg.llama_server_bin = bin;
            }
        }
        if let Ok(repo) = std::env::var("HOLOIROH_LOCAL_MODEL_HF_REPO") {
            if !repo.trim().is_empty() {
                cfg.model_hf_repo = repo;
            }
        }
        if let Ok(port) = std::env::var("HOLOIROH_LOCAL_MODEL_PORT") {
            if let Ok(parsed) = port.trim().parse::<u16>() {
                if parsed != 0 {
                    cfg.port = parsed;
                }
            }
        }
        cfg
    }

    /// The OpenAI-compatible base URL `holo serve`'s agent runtime should point at:
    /// `http://127.0.0.1:<port>/v1`. This is the string passed as `holo serve --base-url <url>`
    /// and set as `HAI_AGENT_RUNTIME_BASE_URL`.
    pub fn base_url(&self) -> String {
        format!("http://{LOOPBACK_HOST}:{}/v1", self.port)
    }

    /// The root health URL polled to decide the server is ready: `http://127.0.0.1:<port>/health`.
    /// `llama-server` serves `/health` at the root (not under `/v1`), returning `200` once the
    /// model is loaded and it can accept requests.
    pub fn health_url(&self) -> String {
        format!("http://{LOOPBACK_HOST}:{}/health", self.port)
    }

    /// The exact argv (after the program name) this config produces for `llama-server`. Kept
    /// separate from [`Self::command`] so the argument list can be asserted/printed on its own,
    /// and so the loopback-only invariant is expressed in one place. Never emits a `--host` other
    /// than [`LOOPBACK_HOST`] and never emits `--no-mmproj` (the vision projector must load).
    pub fn command_args(&self) -> Vec<String> {
        vec![
            "-hf".to_string(),
            self.model_hf_repo.clone(),
            "--host".to_string(),
            LOOPBACK_HOST.to_string(),
            "--port".to_string(),
            self.port.to_string(),
        ]
    }

    /// Build the exact [`Command`] the daemon would spawn for `llama-server`, **without spawning
    /// it**. This is the single source of truth for the subprocess invocation: [`Self::spawn`]
    /// calls it and only then adds the stdio/kill-on-drop settings before `.spawn()`. Inspecting
    /// the returned `Command`'s `get_program()` / `get_args()` (as `examples/local_model_probe.rs`
    /// does) witnesses the full invocation with zero model load.
    pub fn command(&self) -> Command {
        let mut cmd = Command::new(&self.llama_server_bin);
        cmd.args(self.command_args());
        cmd
    }
}

/// A running `llama-server` child process plus the config it was started from.
pub struct LocalModelServer {
    child: Child,
    config: LocalModelConfig,
}

impl LocalModelServer {
    /// Spawn `llama-server` for `config` as a managed subprocess and wait (up to
    /// [`HEALTH_TIMEOUT`]) for `/health` to report ready before returning.
    ///
    /// **Heavy:** this loads the ~21 GB model into memory -- it is a real runtime operation, not
    /// something the build/verification path exercises (see the module doc and
    /// `examples/local_model_probe.rs`, which builds the command via [`LocalModelConfig::command`]
    /// but never calls this).
    pub async fn spawn(config: LocalModelConfig) -> Result<Self> {
        if LOCAL_MODEL_RUNNING
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            bail!(
                "llama-server is already running (tracked child process exists); refusing to spawn a second instance"
            );
        }
        match Self::spawn_inner(config).await {
            Ok(server) => Ok(server),
            Err(err) => {
                LOCAL_MODEL_RUNNING.store(false, Ordering::SeqCst);
                Err(err)
            }
        }
    }

    async fn spawn_inner(config: LocalModelConfig) -> Result<Self> {
        let mut cmd = config.command();
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = cmd.spawn().with_context(|| {
            format!(
                "failed to spawn `{} {}` (is llama.cpp's `llama-server` on PATH?)",
                config.llama_server_bin,
                config.command_args().join(" ")
            )
        })?;

        // Drain llama-server's stdout/stderr into tracing so the pipes never fill and block the
        // child, and so its (verbose) load/serve logs are visible under the daemon's logging.
        if let Some(stdout) = child.stdout.take() {
            spawn_log_drain(stdout, "llama-server", tracing::Level::DEBUG);
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_log_drain(stderr, "llama-server", tracing::Level::INFO);
        }

        wait_for_health(&config, &mut child).await?;
        tracing::info!(
            port = config.port,
            base_url = %config.base_url(),
            "llama-server is healthy (local inference ready)"
        );

        Ok(Self { child, config })
    }

    /// The config this server was started from (its base URL, port, model repo). Diagnostic
    /// accessor for a running server; `main.rs` reaches the base URL via [`Self::base_url`]
    /// instead, so this whole-config getter has no live call site yet -- kept for logging/health
    /// call sites that want the model repo or port without re-deriving them.
    #[allow(dead_code)]
    pub fn config(&self) -> &LocalModelConfig {
        &self.config
    }

    /// The OpenAI-compatible base URL to point `holo serve` at. Convenience over
    /// `self.config().base_url()`.
    pub fn base_url(&self) -> String {
        self.config.base_url()
    }

    /// PID of the `llama-server` process, for diagnostics.
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// Non-blocking liveness check: `Ok(None)` still running, `Ok(Some(status))` already exited.
    ///
    /// Provided as the supervision hook a health-check loop for the local server would poll
    /// (mirroring [`crate::holo_bridge::process::HoloServeProcess::try_wait`], which
    /// `holo_bridge::health` already uses). No such loop restarts `llama-server` today -- the
    /// daemon spawns it once at startup and `holo_bridge::health` supervises the `holo serve`
    /// child, not this one -- so this method is not yet called from the live path. Kept
    /// `#[allow(dead_code)]` rather than removed so adding that loop later needs no API change.
    #[allow(dead_code)]
    pub fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.child.try_wait()
    }

    /// Terminate `llama-server`: SIGTERM, then SIGKILL if it doesn't exit promptly. Preferred
    /// (async, awaitable) shutdown path; [`Drop`] is the synchronous safety net.
    pub async fn shutdown(mut self) -> Result<()> {
        let result = self.terminate_and_wait().await;
        LOCAL_MODEL_RUNNING.store(false, Ordering::SeqCst);
        result
    }

    async fn terminate_and_wait(&mut self) -> Result<()> {
        #[cfg(unix)]
        {
            if let Some(pid) = self.child.id() {
                // SAFETY: libc::kill with a valid pid + SIGTERM is well-defined; ESRCH (already
                // exited) is a harmless no-op, we fall through to the timeout+kill below.
                unsafe {
                    libc::kill(pid as libc::pid_t, libc::SIGTERM);
                }
            }
        }
        match tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await {
            Ok(Ok(status)) => {
                tracing::info!(?status, "llama-server exited after SIGTERM");
                Ok(())
            }
            _ => {
                tracing::warn!("llama-server did not exit within 5s of SIGTERM; killing");
                self.child.kill().await.context("failed to kill llama-server")?;
                Ok(())
            }
        }
    }
}

impl Drop for LocalModelServer {
    /// Best-effort synchronous safety net (SIGTERM + the `Command`'s own `kill_on_drop(true)`) for
    /// the case where [`Self::shutdown`] was never called, and clears the single-instance guard so
    /// the process is never left permanently un-spawnable after an unclean exit. Mirrors
    /// `HoloServeProcess::drop`.
    fn drop(&mut self) {
        LOCAL_MODEL_RUNNING.store(false, Ordering::SeqCst);
        #[cfg(unix)]
        {
            if let Some(pid) = self.child.id() {
                tracing::warn!(
                    pid,
                    "LocalModelServer dropped without shutdown(); sending SIGTERM (kill_on_drop will SIGKILL if needed)"
                );
                // SAFETY: same as terminate_and_wait -- valid pid, well-defined syscall.
                unsafe {
                    libc::kill(pid as libc::pid_t, libc::SIGTERM);
                }
            }
        }
    }
}

async fn wait_for_health(config: &LocalModelConfig, child: &mut Child) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .context("failed to build llama-server health-check HTTP client")?;
    let deadline = Instant::now() + HEALTH_TIMEOUT;
    let health_url = config.health_url();

    loop {
        if let Some(status) = child
            .try_wait()
            .context("failed to poll llama-server child status")?
        {
            bail!("llama-server exited during startup with status {status}");
        }
        if let Ok(resp) = client.get(&health_url).send().await {
            if resp.status().is_success() {
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill().await;
            bail!(
                "llama-server did not become healthy within {:?} (GET {} never returned 2xx). \
                 Loading the ~21 GB GGUF can be slow; increase the timeout or check the model cache.",
                HEALTH_TIMEOUT,
                health_url
            );
        }
        sleep(HEALTH_POLL_INTERVAL).await;
    }
}

/// Pipes a child's stdout/stderr into `tracing` line-by-line. Identical in intent to
/// `holo_bridge::process::spawn_log_drain`.
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
                        tracing::debug!(target: "local_model::child", "{label}: {line}")
                    }
                    _ => tracing::info!(target: "local_model::child", "{label}: {line}"),
                },
                Ok(None) => break,
                Err(err) => {
                    tracing::warn!(target: "local_model::child", "{label}: log drain error: {err}");
                    break;
                }
            }
        }
    });
}
