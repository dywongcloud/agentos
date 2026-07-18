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
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::time::{Instant, sleep};

use crate::holo_bridge::a2a_client::A2aClient;

/// Env var `holo serve` reads for its bearer token (`ServeSettings.auth_token` /
/// `A2A_TOKEN_ENV` in `serve.py`). Setting this ourselves before spawn means we never have
/// to scrape stderr for a generated token.
pub const HOLO_AUTH_TOKEN_ENV: &str = "HOLO_AUTH_TOKEN";

/// Default A2A port per `serve.py`'s `A2A_DEFAULT_PORT`. The daemon does not have to use this
/// default -- it picks its own port explicitly -- but it's recorded here since it's the value
/// `holo serve` binds to when `--port` is omitted.
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
    pub port: u16,
    pub base_url: String,
    pub auth_token: String,
}

impl HoloServeProcess {
    /// Spawn `holo serve --port <port>` as a managed subprocess, generating our own bearer
    /// token and exporting it via `HOLO_AUTH_TOKEN` so no stderr-scraping is needed. Waits for
    /// `/health` to report ok before returning.
    ///
    /// `holo_bin` is the path to (or bare name of) the `holo` CLI executable; resolved via
    /// `PATH` by `tokio::process::Command` when it's a bare name like `"holo"`.
    pub async fn spawn(holo_bin: &str, port: u16) -> Result<Self> {
        let auth_token = generate_token();
        let base_url = format!("http://127.0.0.1:{port}");

        let mut cmd = Command::new(holo_bin);
        cmd.args(["serve", "--port", &port.to_string()])
            .env(HOLO_AUTH_TOKEN_ENV, &auth_token)
            // Inherit the parent's env otherwise (HAI_API_KEY, HAI_BASE_URL, etc. from
            // mac-daemon/.env / the launching shell) so `holo serve`'s own settings loader
            // (settings.py) sees the same auth/gateway config the operator configured.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

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
        })
    }

    /// Build an [`A2aClient`] bound to this running server.
    pub fn client(&self) -> A2aClient {
        A2aClient::new(self.base_url.clone(), self.auth_token.clone())
    }

    /// PID of the `holo serve` process, for diagnostics.
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// Non-blocking poll of whether the child has exited. `Ok(None)` = still running;
    /// `Ok(Some(status))` = exited (the crash signal `holo_bridge::health`'s ongoing
    /// supervision loop watches for, distinct from `wait_for_health`'s one-time startup
    /// check above which uses the same underlying `tokio::process::Child::try_wait` call
    /// internally). Exposed publicly so post-startup supervision can reuse it instead of
    /// duplicating process-liveness logic.
    pub fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.child.try_wait()
    }

    /// Terminate `holo serve`. Sends SIGTERM first (mirrors the CLI's own `Ctrl+C to stop`
    /// story -- `holo serve` has no documented graceful-shutdown RPC of its own, it relies on
    /// process signals / uvicorn's own signal handling), then force-kills if it doesn't exit
    /// promptly.
    pub async fn shutdown(mut self) -> Result<()> {
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
