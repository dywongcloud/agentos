//! Runs the `holo` command-line interface (CLI) `stop` subcommand for the global kill-switch path.
//!
//! ## Source grounding
//!
//! The behavior comes directly from `hcompai/holo-desktop-cli` source.
//! The source files are `src/holo_desktop/cli/stop.py` and `src/holo_desktop/killswitch/channel.py`.
//! They were read through the GitHub application programming interface (API) on 2026-07-17.
//!
//! ## Graceful stop behavior
//!
//! - `holo stop` does not make a Hypertext Transfer Protocol (HTTP) request.
//! - It writes the current wall-clock time to `~/.holo/stop`.
//! - The value is `time.time()` as plain decimal text.
//! - `killswitch/channel.py::request_stop` performs the write.
//! - The stop channel has no port or authentication.
//! - It is not scoped to a `holo serve` instance.
//! - It is not scoped to an Agent-to-Agent (A2A) `contextId`.
//! - The file is host-wide.
//! - Every in-flight Holo turn on the machine polls it.
//!
//! Every in-flight turn uses the same `session_runner.run_turn` path.
//! These turns include CLI `run`, `serve` A2A, `acp`, and `mcp` surfaces.
//! Each turn runs a `StopWatcher` at a 250-millisecond polling interval.
//! `STOP_POLL_S` defines that interval.
//! `StopSentinel.stop_requested()` accepts only requests filed after the turn's start time.
//! A request filed before a later turn starts does not stop that turn retroactively.
//! A request stops all turns that were already in flight.
//! It cannot target one turn among several concurrent turns.
//!
//! After a turn detects a stop, `run_turn` pauses and then cancels the backend session.
//! `_pause_then_cancel` calls `client.pause(session_id)` before `client.cancel(session_id)`.
//! Both calls are best-effort.
//! The turn records `TrajectoryStatus.INTERRUPTED`.
//! In `serve.py`, `_TERMINAL_TO_A2A` maps that outcome to `TASK_STATE_CANCELED`.
//! The daemon receives a normal terminal `TaskUpdate::Terminal` with `TerminalState::Canceled`.
//! On the wire, this result is indistinguishable from an A2A-native `tasks/cancel` result.
//! Therefore, the daemon requires no special handling.
//!
//! ## Forced stop behavior
//!
//! `holo stop --force` also reads `~/.holo/agent-pid-<port>`.
//! `launcher.py::pid_file_path` and `discover_runtime_pids` implement that lookup.
//! The command sends the non-catchable kill signal (`SIGKILL`) to the discovered process group.
//! This signal targets the `hai-agent-runtime` executable, not `holo serve`.
//! The `holo serve` A2A HTTP server remains running.
//! However, its backend runtime is no longer available.
//! The next prompt fails when the executor's `AgentApiClient` calls return errors.
//! Prompts continue to fail until `holo serve` restarts or reattaches.
//! This module does not restart `holo serve` automatically after a forced stop.
//! The daemon's top-level supervisor makes that decision.
//! An immediate automatic restart could conflict with the operator's explicit force-kill intent.
//!
//! The double-`Esc` keyboard alternative applies only to CLI and interactive use.
//! It does not apply to the daemon because the daemon has no keyboard focus.
//!
//! ## Why this module runs the CLI
//!
//! Writing `~/.holo/stop` directly would reproduce the three Python lines in `request_stop`.
//! That approach would work and avoid a process spawn.
//! However, the task explicitly requested one of two approaches:
//!
//! 1. Run the `holo` CLI `stop` command.
//! 2. Call the equivalent A2A cancellation operation.
//!
//! Running the real `holo stop` command follows future stop-channel format changes.
//! Those changes can include the path, encoding, or replacement of the file mechanism.
//! Reimplementing `request_stop` could silently fail after such a change.
//!
//! `control.rs` and `a2a_client.rs` implement the A2A `tasks/cancel` equivalent separately.
//! That path has a smaller blast radius because it targets one context.
//! Prefer that path when a `context_id` is available.
//! This module provides the fallback global path for `stop everything` and `--force` requests.

use anyhow::{Context, Result, bail};
use tokio::process::Command;

/// Provides the exact argument vector for the `holo` CLI.
///
/// When `force` is `false`, the result is `["stop"]` for graceful pause-then-cancel behavior.
/// When `force` is `true`, the result is `["stop", "--force"]`.
/// Passing the forced form to `holo` also sends `SIGKILL` to the `hai-agent-runtime` process.
/// See this module's documentation and `holo stop --help`.
///
/// This pure function makes kill-switch command construction independently witnessable.
/// `examples/holo_stop_probe.rs` verifies both `force` values directly.
/// The probe does not send `SIGKILL` to a running `hai-agent-runtime`.
/// [`holo_stop`] builds its `Command` from this exact vector.
/// The probe therefore verifies the real invocation.
/// It does not verify a parallel copy that could drift.
pub fn build_stop_args(force: bool) -> Vec<&'static str> {
    if force {
        vec!["stop", "--force"]
    } else {
        vec!["stop"]
    }
}

/// Starts `holo stop` as a child process and waits for it to exit.
///
/// When `force` is `true`, the command includes `--force`.
/// `holo_bin` is the executable path or name that starts `holo serve` in `process.rs`.
/// The `stop` operation is a subcommand of that CLI.
/// It is not a separate executable.
/// The function returns an error when spawning fails.
/// It also returns an error when the child exits unsuccessfully.
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
