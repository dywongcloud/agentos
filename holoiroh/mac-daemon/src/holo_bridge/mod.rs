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
pub mod health;
pub mod process;
pub mod stop;

use anyhow::{Context, Result};
use tokio::sync::mpsc;

pub use control::{ControlEvent, ControlMessage, DoneStatus, HoloControlBridge};
pub use process::HoloServeProcess;

/// One place `holo serve`'s model inference can be pointed: an OpenAI-compatible base URL
/// plus (optionally) the model name to request from it. Two of these exist today:
/// the local `llama-server` (Aro Private mode; no model override -- llama-server ignores the
/// name) and the loopback tinfoil fallback proxy (`crate::tinfoil_proxy`; model `kimi-k2-6`,
/// which tinfoil routes by). `label` is for logs/status events only.
#[derive(Clone, Debug)]
pub struct InferenceTarget {
    pub base_url: String,
    pub model: Option<String>,
    pub label: String,
}

/// Outcome of [`HoloBridge::activate_fallback`], so the caller (the control bridge's
/// rate-limit retry path) can distinguish "switched, retry the turn" from "nothing to
/// switch to / already switched, surface the original failure".
#[derive(Debug)]
pub enum FallbackActivation {
    /// Now running on the fallback backend; the A2A client has been swapped. Retry the turn.
    Switched { label: String },
    /// The fallback backend was already active -- the failure happened ON the fallback.
    AlreadyActive,
    /// No fallback is configured (no TINFOIL_API_KEY, or local-only mode).
    Unavailable,
}

/// Owns the `holo serve` subprocess and the control bridge built on top of it. This is the
/// type `main.rs` constructs once on daemon startup and keeps alive for the process lifetime.
pub struct HoloBridge {
    // `tokio::sync::Mutex` (not `std`) since `holo_bridge::health`'s restart path holds this
    // across an `.await` (respawning the child, which itself awaits `/health`) -- a std Mutex
    // guard can't cross an await point.
    process: tokio::sync::Mutex<HoloServeProcess>,
    holo_bin: String,
    port: u16,
    /// The A2A agent card's `protocolVersion`, captured at startup (and re-verified on
    /// `restart_process`). Surfaced via [`HoloBridge::protocol_version`] so a caller building an
    /// executor over this bridge can report the real backend protocol version in its
    /// capabilities, rather than guessing. `None` when the card did not advertise one.
    ///
    /// `#[allow(dead_code)]`: read by [`crate::executor::start_holo_desktop_executor`] via the
    /// accessor below, which lives in the `executor` module -- compiled into the *lib* target
    /// (and its examples) but not the *bin* target (`main.rs` does not yet route through the
    /// executor seam; see its `mod executor` note), so from the binary crate's isolated
    /// perspective this field is not read. Same lib-vs-bin asymmetry `#[allow(dead_code)]` already
    /// covers elsewhere in this crate.
    #[allow(dead_code)]
    protocol_version: Option<String>,
    /// The backend the daemon was STARTED on: `Some` = the local `llama-server` (alpha,
    /// no-cloud mode); `None` = `holo serve`'s own configured hosted backend. Every
    /// crash-restart re-applies whichever backend is CURRENTLY active (primary or fallback)
    /// -- a restart must never silently fall back from local to cloud, and equally must not
    /// silently hop backends on its own.
    primary: Option<InferenceTarget>,
    /// The rate-limit fallback backend (the loopback tinfoil proxy), when configured.
    /// `None` disables failover entirely -- notably in local (no-cloud) mode, where routing
    /// to a cloud fallback would violate the mode's whole point.
    fallback: Option<InferenceTarget>,
    /// Whether the fallback backend is the active one. Guarded by the `process` slot lock
    /// discipline: only mutated while holding `process` (every backend swap holds it), so a
    /// concurrent restart can never observe a half-switched state. A separate std mutex (not
    /// a field on the tokio-mutexed slot) so sync readers (`is_on_fallback`) don't need the
    /// async lock.
    on_fallback: std::sync::Mutex<bool>,
    /// When the fallback was activated, for the cooldown-based restore to primary.
    fallback_since: std::sync::Mutex<Option<std::time::Instant>>,
    /// How long to stay on the fallback before a new turn probes the primary backend again.
    fallback_cooldown: std::time::Duration,
    /// The per-prompt intent router's current model choice on the HOSTED primary path, when
    /// it has switched away from the primary's own default. `None` = no routed override (the
    /// primary's configured/default model is in effect). Same last-known-good discipline as
    /// `on_fallback`: only set AFTER a successful [`Self::switch_to`], guarded by the same
    /// `process` slot lock, and merged into [`Self::current_target`] so crash-restarts and
    /// fallback-cooldown restores re-apply the routed model instead of silently dropping back
    /// to the primary's default. Deliberately NOT consulted while `on_fallback` is true (the
    /// router never fights the tinfoil failover -- see `router::choose_model`'s doc) or when
    /// `primary` is the local llama-server (a local base URL always spawns with `model: None`;
    /// see [`Self::route_model`]).
    routed_model: std::sync::Mutex<Option<String>>,
    pub control: HoloControlBridge,
}

impl HoloBridge {
    /// Spawn `holo serve` on `port`, wait for it to become healthy, verify its agent card,
    /// and build the control bridge on top of it.
    ///
    /// `holo_bin` is the `holo` CLI executable (bare `"holo"` to resolve via `PATH`, or an
    /// absolute path). `local_base_url` points `holo serve`'s inference at a local
    /// OpenAI-compatible server (alpha's local `llama-server`) when `Some`, and leaves it on its
    /// configured backend when `None` -- see [`HoloServeProcess::build_command`]. `events_tx` is
    /// where translated [`ControlEvent`]s get sent; the caller is expected to forward those out
    /// over the (not-yet-implemented) iroh control stream to the iOS app.
    pub async fn start(
        holo_bin: impl Into<String>,
        port: u16,
        primary: Option<InferenceTarget>,
        fallback: Option<InferenceTarget>,
        fallback_cooldown: std::time::Duration,
        events_tx: mpsc::UnboundedSender<ControlEvent>,
    ) -> Result<Self> {
        let holo_bin = holo_bin.into();
        let process = HoloServeProcess::spawn(
            &holo_bin,
            port,
            primary.as_ref().map(|t| t.base_url.as_str()),
            primary.as_ref().and_then(|t| t.model.as_deref()),
        )
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
        let protocol_version = card.protocol_version.clone();

        let control = HoloControlBridge::new(client, holo_bin.clone(), events_tx);

        Ok(Self {
            process: tokio::sync::Mutex::new(process),
            holo_bin,
            port,
            protocol_version,
            primary,
            fallback,
            on_fallback: std::sync::Mutex::new(false),
            fallback_since: std::sync::Mutex::new(None),
            fallback_cooldown,
            routed_model: std::sync::Mutex::new(None),
            control,
        })
    }

    /// The `(base_url, model)` a (re)spawn should use right now: the fallback target while it
    /// is active, else the primary with the router's current model override merged in (when
    /// the primary is the hosted path -- a `base_url: Some(..)` target, e.g. local llama-server,
    /// never gets a model override, since `switch_to`/`build_command` already treat
    /// `base_url: Some` and `model: Some` as mutually exclusive on the hosted-vs-local axis --
    /// see [`Self::route_model`]'s doc for why). Returned as owned `String`s (not a borrowed
    /// `InferenceTarget`) so a routed model -- which has no `base_url` at all -- can be
    /// expressed without forcing `InferenceTarget::base_url` to become `Option`.
    fn current_target(&self) -> (Option<String>, Option<String>) {
        let on_fallback = *self.on_fallback.lock().expect("on_fallback lock poisoned");
        if on_fallback {
            let Some(t) = &self.fallback else {
                return (None, None);
            };
            return (Some(t.base_url.clone()), t.model.clone());
        }
        let base_url = self.primary.as_ref().map(|t| t.base_url.clone());
        let mut model = self.primary.as_ref().and_then(|t| t.model.clone());
        // The router only ever applies to the HOSTED primary (no base_url of its own): a
        // local-mode primary already ignores model names, so a routed override would be a
        // silent no-op at best and, if `switch_to` ever paired it with a spawn expressing
        // both base_url and model contradictorily, a P0-11 no-cloud-mode hazard at worst.
        if base_url.is_none() {
            if let Some(routed) = self.routed_model.lock().expect("routed_model lock poisoned").clone() {
                model = Some(routed);
            }
        }
        (base_url, model)
    }

    /// Whether the fallback backend is currently active (for status/log surfaces).
    pub fn is_on_fallback(&self) -> bool {
        *self.on_fallback.lock().expect("on_fallback lock poisoned")
    }

    /// The intent router's current model override on the hosted primary path, if any (for
    /// status/log surfaces).
    ///
    /// `#[allow(dead_code)]`: no bin-target caller yet (same lib-vs-bin asymmetry as
    /// `protocol_version` above); kept as real public API for a future status/log surface.
    #[allow(dead_code)]
    pub fn routed_model(&self) -> Option<String> {
        self.routed_model.lock().expect("routed_model lock poisoned").clone()
    }

    /// Swap the running `holo serve` onto `(base_url, model)` (terminate live child -> spawn
    /// replacement -> verify agent card -> swap slot + A2A client). The whole swap holds the
    /// `process` slot lock, same discipline as [`Self::restart_process`]. On spawn failure the
    /// dead child stays in the slot; the health loop notices and restarts onto whatever backend
    /// is marked active -- `on_fallback`/`routed_model` are only flipped AFTER a successful
    /// swap, so a failed switch self-heals back onto the backend that was last known good.
    ///
    /// `going_to_fallback` and `new_routed_model` are the state-flip instructions applied only
    /// on success; `new_routed_model` is `Some(None)` to explicitly clear a routed override
    /// (switching back to the primary's default), `None` to leave `routed_model` untouched
    /// (the fallback-activation and restore-to-primary call sites, which don't touch routing).
    async fn switch_to(
        &self,
        base_url: Option<&str>,
        model: Option<&str>,
        going_to_fallback: bool,
        new_routed_model: Option<Option<String>>,
    ) -> Result<()> {
        let mut slot = self.process.lock().await;

        if let Err(err) = slot.terminate_in_place().await {
            // Non-fatal: the spawn below is the real gate. A child that refused to die will
            // make the port preflight fail with a clear message.
            tracing::warn!(error = %format!("{err:#}"), "terminating holo serve for backend switch failed");
        }

        let new_process = HoloServeProcess::spawn(&self.holo_bin, self.port, base_url, model)
            .await
            .context("failed to respawn holo serve on the new backend")?;
        let client = new_process.client();
        client
            .probe_agent_card()
            .await
            .context("holo serve on the new backend did not present a valid A2A agent card")?;

        let _old = std::mem::replace(&mut *slot, new_process);
        *self.on_fallback.lock().expect("on_fallback lock poisoned") = going_to_fallback;
        *self.fallback_since.lock().expect("fallback_since lock poisoned") =
            going_to_fallback.then(std::time::Instant::now);
        if let Some(routed) = new_routed_model {
            *self.routed_model.lock().expect("routed_model lock poisoned") = routed;
        }
        drop(slot);
        self.control.replace_client(client);
        Ok(())
    }

    /// Fail over to the fallback backend (tinfoil), if configured and not already active.
    /// Called by the control bridge when a turn dies with a backend-error shape (the hosted
    /// backend's 429s surface as `holo serve`'s generic "agent backend error" -- see
    /// `HoloControlBridge`'s retry path).
    pub async fn activate_fallback(&self) -> Result<FallbackActivation> {
        let Some(target) = self.fallback.clone() else {
            return Ok(FallbackActivation::Unavailable);
        };
        if self.is_on_fallback() {
            return Ok(FallbackActivation::AlreadyActive);
        }
        tracing::warn!(fallback = %target.label, "switching holo serve to the fallback inference backend");
        self.switch_to(Some(&target.base_url), target.model.as_deref(), true, None)
            .await?;
        Ok(FallbackActivation::Switched {
            label: target.label,
        })
    }

    /// If the fallback has been active longer than the cooldown, switch back to the primary
    /// backend so the (presumably no-longer-rate-limited) hosted path gets probed again.
    /// Called at the start of each new turn. If the hosted backend is still rate-limited,
    /// that turn's failure re-triggers [`Self::activate_fallback`] -- one failed attempt per
    /// cooldown window is the probe cost, and the turn itself still completes via the retry.
    /// Restores whatever model the router had last selected on the primary (`current_target`
    /// merges `routed_model` in), not the primary's bare default. Returns `true` when a
    /// restore actually happened.
    pub async fn maybe_restore_primary(&self) -> bool {
        if !self.is_on_fallback() {
            return false;
        }
        let elapsed = self
            .fallback_since
            .lock()
            .expect("fallback_since lock poisoned")
            .map(|since| since.elapsed());
        match elapsed {
            Some(elapsed) if elapsed >= self.fallback_cooldown => {
                tracing::info!(
                    ?elapsed,
                    "fallback cooldown elapsed; switching holo serve back to the primary backend"
                );
                // The primary's own base_url plus whatever model the router last selected
                // there (merged in directly, not via `current_target()`: that method reads
                // `on_fallback`, which is still `true` here -- it only flips inside
                // `switch_to`, after this spawn succeeds).
                let base_url = self.primary.as_ref().map(|t| t.base_url.clone());
                let model = if base_url.is_none() {
                    self.routed_model.lock().expect("routed_model lock poisoned").clone()
                } else {
                    self.primary.as_ref().and_then(|t| t.model.clone())
                };
                match self
                    .switch_to(base_url.as_deref(), model.as_deref(), false, None)
                    .await
                {
                    Ok(()) => true,
                    Err(err) => {
                        tracing::warn!(
                            error = %format!("{err:#}"),
                            "restore to primary backend failed; staying on fallback"
                        );
                        false
                    }
                }
            }
            _ => false,
        }
    }

    /// Per-prompt intent routing on the hosted primary path: classify `prompt_text` and, if
    /// the decision differs (with hysteresis) from the currently active model, respawn `holo
    /// serve` onto it. No-ops (returns `Ok(())` immediately) when: local (no-cloud) mode is
    /// active (`primary` has its own `base_url`, i.e. the local llama-server -- routing a
    /// model NAME at a local server is meaningless, and P0-11 requires the alpha to never
    /// introduce a hosted-shaped spawn there), or the fallback is currently active (the router
    /// must never fight the tinfoil failover -- `maybe_restore_primary` re-applies the routed
    /// model once the cooldown elapses and hosted service resumes). See `router::classify`.
    pub async fn route_model(&self, prompt_text: &str) -> Result<()> {
        // `primary: Some(..)` means local (no-cloud) mode -- `main.rs` only ever builds a
        // primary `InferenceTarget` for the local llama-server; the hosted path leaves
        // `primary: None` so `holo serve` keeps its own configured backend. Routing a model
        // NAME only makes sense on that hosted path.
        if self.primary.is_some() {
            return Ok(());
        }
        if self.is_on_fallback() {
            return Ok(());
        }
        let active = self.routed_model.lock().expect("routed_model lock poisoned").clone();
        let active_tier = active
            .as_deref()
            .and_then(crate::router::Tier::from_model_id)
            .unwrap_or(crate::router::Tier::Simple);
        let Some(new_tier) = crate::router::should_switch(active_tier, prompt_text) else {
            return Ok(());
        };
        let target_model = new_tier.model_id();
        tracing::info!(
            from = ?active_tier,
            to = ?new_tier,
            model = target_model,
            "intent router switching holo serve's model"
        );
        self.switch_to(None, Some(target_model), false, Some(Some(target_model.to_string())))
            .await
    }

    /// The A2A agent card's `protocolVersion` this bridge's `holo serve` advertised at startup,
    /// or `None` if it did not advertise one. See the `protocol_version` field. Used by
    /// [`crate::executor::start_holo_desktop_executor`] to report the real backend version in
    /// [`crate::executor::ExecutorCapabilities`].
    ///
    /// `#[allow(dead_code)]` for the same lib-vs-bin reason as the field it reads (the only
    /// caller lives in the `executor` module, absent from the bin target).
    #[allow(dead_code)]
    pub fn protocol_version(&self) -> Option<&str> {
        self.protocol_version.as_deref()
    }

    /// Non-blocking liveness check on the supervised `holo serve` child. See
    /// [`HoloServeProcess::try_wait`] and `holo_bridge::health`, the only caller.
    pub async fn try_wait_process(&self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.process.lock().await.try_wait()
    }

    /// Replace a dead `holo serve` child with a freshly-spawned one, and rebuild
    /// [`HoloControlBridge`]'s internal A2A client to point at it. Does NOT touch `self.control`'s
    /// event sink, busy/queue state, or anything else about the bridge's identity -- only the
    /// underlying process and the client's connection to it change, so in-flight callers holding
    /// a reference to this `HoloBridge` are unaffected beyond their next A2A call going to the
    /// new process. See `holo_bridge::health`'s module doc for why this can never reach the iroh
    /// P2P session (this type has no field referencing it).
    pub async fn restart_process(&self) -> Result<()> {
        // Take the slot lock for the WHOLE swap (disarm -> spawn -> replace): the health loop's
        // `try_wait_process` (the only other lock taker on a hot path) blocks until the restart
        // settles instead of racing a half-restarted state, and no concurrent restart can
        // interleave. The spawn's health wait makes this a long hold; that is deliberate.
        let mut slot = self.process.lock().await;

        // Disarm the dead child's single-instance guard claim BEFORE spawning the replacement.
        // Without this, `GuardClaim::try_acquire` inside `spawn` fails against the dead
        // process's still-held claim, deterministically, every attempt -- the exact
        // "restart failed: failed to respawn holo serve" every-tick loop witnessed live.
        // (The child is confirmed exited -- that's why we're here -- so releasing its claim
        // cannot enable a second LIVE instance.)
        slot.disarm_guard();

        // Re-apply the inference config of whichever backend is CURRENTLY active (primary, or
        // the fallback if a rate-limit failover switched to it): a crash-restart must never
        // silently drop from local (no-cloud) back to a hosted backend, and equally must not
        // undo an active failover. On failure the `?` releases the lock with the disarmed dead
        // process still in the slot -- the next health tick observes it exited and retries
        // this whole path (disarm is then a no-op).
        let (base_url, model) = self.current_target();
        let new_process = HoloServeProcess::spawn(
            &self.holo_bin,
            self.port,
            base_url.as_deref(),
            model.as_deref(),
        )
        .await
        .context("failed to respawn holo serve")?;
        let client = new_process.client();
        client
            .probe_agent_card()
            .await
            .context("respawned holo serve did not present a valid/compatible A2A agent card")?;

        // Old process's `Drop` (best-effort SIGTERM + kill_on_drop) runs here when `old` goes
        // out of scope -- it already exited (that's why we're restarting), so this is a no-op
        // safety net, not a real termination. Disarmed above, its drop cannot release the
        // guard claim the new process now holds.
        let _old = std::mem::replace(&mut *slot, new_process);
        drop(slot);
        self.control.replace_client(client);
        Ok(())
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

    /// Whether a task is currently PARKED (paused) awaiting `Resume`. See
    /// [`HoloControlBridge::is_paused`] -- surfaced through the control channel's
    /// on-connect greeting so a reconnecting peer restores the Pause/Stop pill
    /// even for a paused task (which `busy_state` alone reports as not busy).
    pub fn is_paused(&self) -> bool {
        self.control.is_paused()
    }

    /// Whether the current pause was created by cooperative auto-yield. See
    /// [`HoloControlBridge::is_auto_yielded`].
    pub fn is_auto_yielded(&self) -> bool {
        self.control.is_auto_yielded()
    }

    /// True while the user is in hands-on remote control. See
    /// [`HoloControlBridge::is_remote_control_active`].
    pub fn is_remote_control_active(&self) -> bool {
        self.control.is_remote_control_active()
    }

    /// Step the running turn aside because the user is active. See
    /// [`HoloControlBridge::auto_yield_pause`].
    pub async fn auto_yield_pause(&self) {
        self.control.auto_yield_pause().await
    }

    /// Resume an auto-yielded turn once the user is idle. See
    /// [`HoloControlBridge::auto_yield_resume`].
    pub async fn auto_yield_resume(&self) {
        self.control.auto_yield_resume().await
    }

    /// PID of the managed `holo serve` process, for diagnostics/health reporting.
    pub async fn holo_serve_pid(&self) -> Option<u32> {
        self.process.lock().await.pid()
    }

    /// Shut down the managed `holo serve` subprocess (SIGTERM, then SIGKILL after a grace
    /// period). Call this during daemon shutdown so `holo serve` (and, transitively, the
    /// `hai-agent-runtime` process it manages) doesn't outlive the daemon as an orphan.
    ///
    /// This is the preferred shutdown path (it awaits graceful exit); `HoloBridge` does not
    /// implement its own `Drop` beyond what its fields already do -- `self.process` is a
    /// [`HoloServeProcess`], whose own `Drop` impl (see `process.rs`) is the synchronous
    /// safety net for the case where this method is never reached (e.g. `main.rs`'s
    /// `Arc::try_unwrap(bridge)` failing because another clone is still alive elsewhere, or a
    /// panic during shutdown).
    pub async fn shutdown(self) -> Result<()> {
        self.process.into_inner().shutdown().await
    }
}
