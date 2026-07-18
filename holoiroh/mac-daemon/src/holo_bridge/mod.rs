//! Bridges the daemon's control channel to H Company's `holo-desktop-cli`.
//!
//! ## Summary of the approach (see submodules for full source citations)
//!
//! On daemon startup, [`HoloBridge::start`] spawns `holo serve --port <N>` as a managed
//! `std::process`-family subprocess (via `tokio::process::Command`, the async equivalent used
//! throughout this async daemon) -- see [`process`]. `holo serve` is H Company's own A2A
//! (Agent2Agent Protocol) HTTP server, fronting the closed-source `hai-agent-runtime` binary
//! that actually runs the Holo3 desktop agent; the daemon does not talk to
//! `hai-agent-runtime` or its Python `AgentApiClient` directly -- those are internal to
//! `holo serve` itself. Once `holo serve`'s `/health` reports ok, incoming control-channel
//! `prompt` / `voice_transcript` messages are submitted to it over its `/a2a` JSON-RPC
//! endpoint via [`a2a_client::A2aClient`], streaming back task-progress/status as
//! [`control::ControlEvent`]s -- see [`control`]. `stop` control messages are handled by a
//! combination of the A2A `tasks/cancel`-equivalent (scoped to one context, preferred) and
//! shelling out to the real `holo stop` CLI command (global kill switch, used when no context
//! is given or `--force` is requested) -- see [`stop`].
//!
//! ## What could not be confirmed from real sources, and how that gap was handled
//!
//! The task instructions require reporting exactly this, so it is collected here rather than
//! scattered:
//!
//! 1. **The A2A JSON-RPC method names and SSE envelope shape** (`message/stream` request
//!    body, `text/event-stream` framing where each `data:` line is one JSON-RPC response,
//!    `TaskStatusUpdateEvent`/`TaskArtifactUpdateEvent` field names like `kind`, `status`,
//!    `artifact`, `parts`). `holo-desktop-cli`'s own source confirms *that* `holo serve` is a
//!    stock `a2a-sdk>=1.0.3` server speaking A2A protocol version `0.3.0` over JSON-RPC at
//!    `/a2a` with `streaming=True` -- but the RPC dispatch and event serialization logic
//!    itself lives inside the `a2a-sdk` PyPI package, which was not fetched (it is not part
//!    of the `holo-desktop-cli` repo). The exact method/field names implemented in
//!    `a2a_client.rs` come from the public, versioned A2A Protocol specification at
//!    <https://a2a-protocol.org>, matched to the exact protocol version (`0.3.0`) and binding
//!    (`JSONRPC`) `holo serve`'s own agent card declares -- not invented, but also not
//!    independently re-derived from source the way the rest of the integration is.
//!    **Mitigation:** [`a2a_client::A2aClient::probe_agent_card`] fetches and checks the
//!    agent card before any RPC call, so a version/binding mismatch fails loudly at startup
//!    instead of the daemon silently sending requests a mismatched server can't parse.
//! 2. **The exact shape of a backend `TrajectoryEvent`** (the `PolicyEvent` /
//!    `ToolResultEvent` / `AnswerEvent` / `ErrorEvent` union `agp_types`/`agent_interface`
//!    define). These packages are the closed-source-adjacent `hai-agent-api` contract
//!    package on PyPI, not part of this repo; not fetched. **Mitigation:** rather than invent
//!    a typed Rust shape for something not confirmed, [`a2a_client::TaskUpdate::Working`]
//!    carries the raw event as an opaque `serde_json::Value` (`raw_event`) alongside any
//!    plain-text status line `holo serve` also attaches, and the control-channel layer
//!    forwards that JSON verbatim to the iOS app rather than guessing at a schema for it.
//! 3. **Whether `GET /.well-known/agent-card.json` is exactly the path `a2a-sdk`'s
//!    `create_agent_card_routes` helper mounts.** Confirmed as the call site in `serve.py`
//!    (`create_agent_card_routes(card)`), and the path itself is the public A2A spec's
//!    documented well-known discovery path, but the helper's exact route registration was not
//!    independently traced into `a2a-sdk` source. If wrong, [`A2aClient::probe_agent_card`]
//!    fails with a clear "GET ... failed" error rather than a confusing downstream JSON-RPC
//!    failure.
//! 4. **`tasks/cancel`'s exact id parameter.** The public spec keys cancellation by A2A
//!    `Task.id`, but this bridge's continuity key (matching `HoloExecutor._sessions`) is
//!    `contextId`, and no A2A `Task.id` is captured separately in the streaming loop today
//!    (only `contextId` is read off each frame). [`a2a_client::A2aClient::cancel`] therefore
//!    passes `context_id` as the cancel id when no task id is supplied, which is confirmed to
//!    reach the right server-side effect for *this specific backend*
//!    (`HoloExecutor.cancel(context, ...)` resolves its `Session` via `context.context_id`,
//!    not via the task id, per `serve.py`) even though it is not the literal spec-documented
//!    lookup key for a generic A2A server. Documented in `a2a_client.rs::cancel`.
//! 5. **iOS-side / control-channel wire framing.** Out of scope for this confirmation list
//!    in the sense that it isn't a `holo-desktop-cli` question at all. The wire framing itself
//!    now lives in `crate::control_channel` (`ClientMessage`/`ServerMessage` over a dedicated
//!    `iroh` ALPN, documented in `holoiroh/PROTOCOL.md`) -- the iOS *app* side (actually
//!    sending/receiving that JSON from Swift) is still not implemented, per
//!    `holoiroh/README.md`. [`control::ControlMessage`] / [`control::ControlEvent`] define the
//!    message contract this module expects/produces as plain `serde`-tagged JSON,
//!    transport-agnostic; `control_channel::ControlChannel` is the adapter between that and
//!    the actual `iroh` stream.
//!
//! Nothing above was implemented on a guess presented as fact: every wire-shape decision that
//! isn't independently confirmed is called out at its point of use, not just here.

pub mod a2a_client;
pub mod control;
pub mod process;
pub mod stop;

use anyhow::{Context, Result};
use tokio::sync::mpsc;

pub use control::{ControlEvent, ControlMessage, HoloControlBridge};
pub use process::HoloServeProcess;

/// Owns the `holo serve` subprocess and the control bridge built on top of it. This is the
/// type `main.rs` constructs once on daemon startup and keeps alive for the process lifetime.
pub struct HoloBridge {
    process: HoloServeProcess,
    pub control: HoloControlBridge,
}

impl HoloBridge {
    /// Spawn `holo serve` on `port`, wait for it to become healthy, verify its agent card,
    /// and build the control bridge on top of it.
    ///
    /// `holo_bin` is the `holo` CLI executable (bare `"holo"` to resolve via `PATH`, or an
    /// absolute path). `events_tx` is where translated [`ControlEvent`]s get sent; the
    /// caller is expected to forward those out over the (not-yet-implemented) iroh control
    /// stream to the iOS app.
    pub async fn start(
        holo_bin: impl Into<String>,
        port: u16,
        events_tx: mpsc::UnboundedSender<ControlEvent>,
    ) -> Result<Self> {
        let holo_bin = holo_bin.into();
        let process = HoloServeProcess::spawn(&holo_bin, port)
            .await
            .context("failed to start holo serve")?;

        let client = process.client();
        let card = client
            .probe_agent_card()
            .await
            .context("holo serve did not present a valid/compatible A2A agent card")?;
        tracing::info!(
            streaming = card.streaming,
            protocol_version = ?card.protocol_version,
            "holo serve agent card verified"
        );

        let control = HoloControlBridge::new(client, holo_bin, events_tx);

        Ok(Self { process, control })
    }

    /// Handle one incoming control-channel message (`prompt` / `voice_transcript` / `stop`).
    pub async fn handle_message(&self, message: ControlMessage) {
        self.control.handle(message).await;
    }

    /// Redirects where this bridge's [`ControlEvent`]s are sent. See
    /// [`HoloControlBridge::replace_event_sink`] -- used by
    /// `crate::control_channel::ControlChannel` to point the bridge at the
    /// currently-connected peer's writer task each time a new
    /// control-channel connection is accepted.
    pub fn replace_event_sink(&self, events_tx: mpsc::UnboundedSender<ControlEvent>) {
        self.control.replace_event_sink(events_tx);
    }

    /// `(turn currently in flight, prompts queued behind it)`. See
    /// [`HoloControlBridge::busy_state`] -- surfaced through the control channel's
    /// on-connect greeting so a reconnecting peer immediately learns whether a stale
    /// in-flight/queued turn survived the drop, without needing to wait for the next
    /// [`ControlEvent`].
    pub fn busy_state(&self) -> (bool, usize) {
        self.control.busy_state()
    }

    /// PID of the managed `holo serve` process, for diagnostics/health reporting.
    pub fn holo_serve_pid(&self) -> Option<u32> {
        self.process.pid()
    }

    /// Shut down the managed `holo serve` subprocess (SIGTERM, then SIGKILL after a grace
    /// period). Call this during daemon shutdown so `holo serve` (and, transitively, the
    /// `hai-agent-runtime` process it manages) doesn't outlive the daemon as an orphan.
    pub async fn shutdown(self) -> Result<()> {
        self.process.shutdown().await
    }
}
