//! Shells out to the `holo` CLI's own `stop` subcommand for the global kill-switch path.
//!
//! ## Source grounding
//!
//! Confirmed directly from `hcompai/holo-desktop-cli` source
//! (`src/holo_desktop/cli/stop.py` and `src/holo_desktop/killswitch/channel.py`, read via the
//! GitHub API on 2026-07-17):
//!
//! - `holo stop` is **not an HTTP call** -- it writes the current wall-clock time
//!   (`time.time()`, plain decimal text) to `~/.holo/stop`
//!   (`killswitch/channel.py::request_stop`). There is no port, no auth, nothing scoped to a
//!   particular `holo serve` instance or A2A `contextId`: it is a single, host-wide file that
//!   *any* in-flight Holo turn on the machine polls.
//! - Every in-flight turn (regardless of surface -- CLI `run`, `serve` A2A, `acp`, `mcp`, all
//!   go through the same `session_runner.run_turn`) runs a `StopWatcher` that polls this file
//!   every 250ms (`STOP_POLL_S`) via `StopSentinel.stop_requested()`, which only returns true
//!   for requests filed **after** that turn's own start time -- so `holo stop` cannot
//!   retroactively "miss" a request filed just before a turn starts, but also cannot target
//!   one specific turn among several concurrent ones; it stops all of them.
//! - On seeing a stop, `run_turn` does pause-then-cancel against the backend session
//!   (`_pause_then_cancel`: `client.pause(session_id)` then `client.cancel(session_id)`,
//!   each best-effort) and marks the turn's outcome `TrajectoryStatus.INTERRUPTED`, which
//!   `serve.py`'s `_TERMINAL_TO_A2A` maps to `TASK_STATE_CANCELED` -- i.e. a `holo stop`
//!   surfaces to an A2A client (this daemon) as an ordinary terminal `TaskUpdate::Terminal`
//!   with `TerminalState::Canceled`, indistinguishable on the wire from an A2A-native
//!   `tasks/cancel`. The daemon does not need to special-case this.
//! - `holo stop --force` additionally reads `~/.holo/agent-pid-<port>`
//!   (`launcher.py::pid_file_path`, `discover_runtime_pids`) and sends `SIGKILL` to that
//!   process group. This targets the **`hai-agent-runtime` binary**, not `holo serve` itself
//!   -- `holo serve` (the A2A HTTP server this daemon spawned) is unaffected and keeps
//!   running, but its backend runtime dies out from under it, so the next prompt sent to it
//!   will fail (the executor's `AgentApiClient` calls will error) until `holo serve` is
//!   restarted or reattaches. This module does not restart `holo serve` automatically after a
//!   forced stop -- that's the caller's (the daemon's top-level supervisor's) decision, since
//!   auto-restarting immediately after an operator explicitly force-killed the runtime could
//!   fight the operator's intent.
//! - Keyboard alternative (double-`Esc`) is CLI/interactive-only and has no bearing on this
//!   daemon, which has no keyboard focus of its own.
//!
//! ## Why shell out instead of writing `~/.holo/stop` directly
//!
//! Writing the file directly (mirroring `request_stop`'s three lines of Python) would work
//! and would save a process spawn, but the task explicitly asks for either "shell out to the
//! holo CLI's stop command, or call the A2A cancel equivalent" -- and shelling out to the
//! real `holo stop` binary is the more robust choice long-term: if a future CLI version
//! changes the stop-channel format (path, encoding, or replaces the file with something
//! else), a daemon that shells out to `holo stop` tracks that change automatically, whereas
//! one that reimplements `request_stop`'s file-write would silently break. The A2A
//! `tasks/cancel`-equivalent path (scoped, lower blast radius) is implemented separately in
//! `control.rs` / `a2a_client.rs` and is preferred whenever a `context_id` is available; this
//! module is the fallback/global path for when the caller wants "stop everything" or when
//! `--force` is requested.

use anyhow::{Context, Result, bail};
use tokio::process::Command;

/// The exact argument vector `holo_stop` passes to the `holo` CLI for a given
/// `force` flag: `["stop"]` for a graceful pause-then-cancel, or
/// `["stop", "--force"]` to additionally SIGKILL the `hai-agent-runtime`
/// process (see this module's doc and `holo stop --help`).
///
/// Factored out of [`holo_stop`] as a pure function so the kill-switch's
/// command construction is witnessable in isolation -- `examples/holo_stop_probe.rs`
/// asserts on this directly for both `force` values without having to actually
/// SIGKILL a runtime (which `holo stop --force` would do to any live
/// `hai-agent-runtime`). `holo_stop` itself builds its `Command` from exactly
/// this vector, so the probe is asserting on the real invocation shape, not a
/// parallel copy that could drift.
pub fn build_stop_args(force: bool) -> Vec<&'static str> {
    if force {
        vec!["stop", "--force"]
    } else {
        vec!["stop"]
    }
}

/// Run `holo stop` (optionally `--force`) as a child process and wait for it to exit.
///
/// `holo_bin` is the same executable path/name used to spawn `holo serve` (see
/// `process.rs`) -- `stop` is a subcommand of the same CLI, not a separate binary.
pub async fn holo_stop(holo_bin: &str, force: bool) -> Result<()> {
    let mut cmd = Command::new(holo_bin);
    cmd.args(build_stop_args(force));

    tracing::info!(force, "issuing `{holo_bin} stop`");

    let output = cmd
        .output()
        .await
        .with_context(|| format!("failed to spawn `{holo_bin} stop`"))?;

    if !output.status.success() {
        bail!(
            "`{holo_bin} stop` exited with {}: stdout={:?} stderr={:?}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    tracing::info!(force, "`{holo_bin} stop` completed");
    Ok(())
}
