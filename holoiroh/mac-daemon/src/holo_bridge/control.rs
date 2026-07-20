//! Control-channel message types and the bridge that dispatches them to `holo serve`'s A2A
//! endpoint, per the architecture described in `holoiroh/README.md` ("Control channel"
//! section): text prompts and voice transcripts flow *into* the daemon from the iOS app,
//! status/log/ack events flow back *out*.
//!
//! ## What is and isn't defined elsewhere
//!
//! The wire format of the control channel itself (how messages are framed over the `iroh`
//! control stream) is defined separately in `crate::control_channel` and
//! `holoiroh/PROTOCOL.md` -- a minimal `ClientMessage`/`ServerMessage` schema
//! (`{type, text?}`, no correlation ids) carried over a dedicated ALPN on the same `iroh`
//! `Endpoint` as the media broadcast. This module's `ControlMessage` / `ControlEvent` are a
//! *richer, internal* schema (`serde`-tagged enums, transport-agnostic) correlated by
//! `request_id`/`context_id` for talking to `holo serve`'s A2A endpoint; `control_channel`
//! translates between the two at the transport boundary (see its module doc for the mapping)
//! rather than this module depending on `iroh` or wire framing at all. `prompt` /
//! `voice_transcript` / `stop` are exactly the three message kinds named in the task;
//! `voice_transcript` is modeled identically to `prompt` (both become an A2A `message/stream`
//! call with the transcript/prompt text as the message body) since `README.md` §"iOS-side" is
//! explicit that "voice input is transcribed ... before being sent as text over the control
//! channel, so the wire format is always a text prompt plus metadata, never raw audio" -- i.e.
//! by the time either message kind reaches this bridge, both are just text.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::{Mutex, RwLock};
use tokio::sync::mpsc;

use crate::holo_bridge::a2a_client::{A2aClient, TaskUpdate, TerminalState};
use crate::limits::ActionCounter;

/// One incoming control-channel message, keyed by the `type` discriminator the task
/// description names explicitly (`"prompt"`, `"voice_transcript"`, `"stop"`).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlMessage {
    /// A typed text prompt from the iOS app's text field.
    Prompt {
        /// Free-form id the caller can use to correlate this prompt with the
        /// `ControlEvent`s it produces; echoed back on every event derived from it.
        request_id: String,
        text: String,
        /// Continue an existing A2A/`hai-agent-runtime` session (see `a2a_client` module doc
        /// on `contextId`), or start a new one when absent.
        #[serde(default)]
        context_id: Option<String>,
    },
    /// A transcribed voice instruction. Same handling as `Prompt` (see module doc) -- kept as
    /// a distinct variant rather than collapsed into `Prompt` so the iOS app / future control
    /// channel can still distinguish "typed" vs. "spoken" provenance for UI/logging purposes,
    /// without that distinction leaking into how the daemon talks to `holo serve`.
    VoiceTranscript {
        request_id: String,
        text: String,
        #[serde(default)]
        context_id: Option<String>,
        /// Optional transcription confidence, if the on-device/service transcriber supplies
        /// one; purely informational, never gates whether the daemon acts on it.
        #[serde(default)]
        confidence: Option<f32>,
    },
    /// Stop the in-flight turn. `context_id` scopes the stop to one A2A task via the A2A
    /// `tasks/cancel` equivalent; when absent, this also engages the CLI-level `holo stop`
    /// kill switch (global to the `holo serve`-spawned runtime, matching the double-Esc /
    /// `holo stop` behavior documented in the upstream CLI -- see `stop.rs` module doc).
    Stop {
        request_id: String,
        #[serde(default)]
        context_id: Option<String>,
        /// Mirrors `holo stop --force`: after requesting the graceful pause-then-cancel,
        /// also SIGKILL the underlying `hai-agent-runtime` process. Use only when the
        /// graceful path is known to be stuck; it ends every in-flight session, not just
        /// this `context_id`.
        #[serde(default)]
        force: bool,
    },
}

impl ControlMessage {
    /// Not read internally today (`HoloControlBridge::handle` destructures each variant
    /// itself), but a natural accessor for any future caller that wants to log/correlate a
    /// message before dispatching it -- kept rather than removed just to silence dead-code.
    #[allow(dead_code)]
    pub fn request_id(&self) -> &str {
        match self {
            ControlMessage::Prompt { request_id, .. }
            | ControlMessage::VoiceTranscript { request_id, .. }
            | ControlMessage::Stop { request_id, .. } => request_id,
        }
    }
}

/// One outgoing control-channel event, reporting progress/status back to the iOS app for a
/// given `request_id`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlEvent {
    /// Acknowledges receipt before any A2A round trip completes, so the UI can show
    /// "sent" immediately rather than waiting on the first task-progress event.
    Ack { request_id: String },
    /// One step of agent progress. `raw_event`, when present, is the backend
    /// `TrajectoryEvent` forwarded verbatim (opaque JSON -- see `a2a_client` module doc on
    /// why this bridge does not attempt to type it); `text`, when present, is a
    /// human-readable status line extracted from the A2A status message.
    Progress {
        request_id: String,
        context_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        raw_event: Option<Value>,
    },
    /// Final answer text for the turn.
    Answer {
        request_id: String,
        context_id: Option<String>,
        text: String,
    },
    /// Terminal status: completed, failed, or canceled.
    Done {
        request_id: String,
        context_id: Option<String>,
        status: DoneStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    /// The bridge itself hit an error before/without a well-formed A2A terminal event (e.g.
    /// `holo serve` unreachable, malformed control message).
    Error { request_id: String, message: String },
    /// A `Prompt`/`VoiceTranscript` arrived while a previous turn was still in flight and was
    /// queued rather than raced against it or dropped. `ahead` is the number of prompts already
    /// queued in front of this one (0 means "runs as soon as the current turn finishes").
    /// `crate::control_channel::ServerMessage::from_control_event` maps this to the wire
    /// `{"type":"status","text":"queued, N ahead"}` the task asks for.
    Queued { request_id: String, ahead: usize },
    /// Out-of-band daemon lifecycle status, not tied to any single request -- e.g.
    /// `holo_bridge::health`'s crash-detected/restarting/restarted notifications. Carries no
    /// `request_id` since it isn't a response to a specific prompt.
    DaemonStatus { text: String },
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DoneStatus {
    Completed,
    Failed,
    Canceled,
}

impl From<TerminalState> for DoneStatus {
    fn from(state: TerminalState) -> Self {
        match state {
            TerminalState::Completed => DoneStatus::Completed,
            TerminalState::Failed => DoneStatus::Failed,
            TerminalState::Canceled => DoneStatus::Canceled,
        }
    }
}

/// One `Prompt`/`VoiceTranscript` turn waiting for the in-flight turn ahead of it to finish.
/// Both `ControlMessage` variants that carry free-form text collapse to this one shape once
/// queued -- the queue only needs to replay `send_and_stream`'s inputs, not the original
/// wire-level distinction (which was already informational-only, see `ControlMessage` doc).
struct QueuedPrompt {
    request_id: String,
    text: String,
    context_id: Option<String>,
}

/// Bridges control-channel messages to `holo serve`'s A2A endpoint and CLI-level stop
/// handling. Holds no transport of its own -- the caller owns receiving `ControlMessage`s
/// (from whatever the eventual `iroh` control stream deserializes into them) and sending
/// `ControlEvent`s back out; this type only owns the A2A/CLI interaction and the
/// prompt-to-context continuity.
pub struct HoloControlBridge {
    /// `std::sync::RwLock` (not `tokio::sync::RwLock`) matching `events_tx` below -- `client`
    /// is only ever swapped wholesale (`replace_client`, on `holo_bridge::health` restart), a
    /// cheap `Clone` of a small struct, never held across an `.await`.
    client: RwLock<A2aClient>,
    /// Wrapped in a `std::sync::RwLock` (not `tokio::sync::RwLock`) so the
    /// active event sink can be swapped per control-channel connection
    /// (see `crate::control_channel`, which calls
    /// `HoloBridge::replace_event_sink` once per accepted connection)
    /// while `emit` stays synchronous -- `emit` is called from inside the
    /// synchronous `FnMut(TaskUpdate)` callback `A2aClient::send_and_stream`
    /// takes, so it cannot `.await`. A std lock is safe here because both
    /// the read (clone a `Sender`, itself a cheap non-blocking op) and the
    /// write (swap a `Sender`) critical sections are tiny and never hold
    /// the lock across an `.await` point. This is also exactly the
    /// mechanism a reconnect relies on: a brand-new control-channel
    /// connection calls `replace_event_sink` on accept, so any turn that
    /// was already streaming (from a prompt submitted on a now-dropped
    /// connection, or drained from the queue below) has its *next* `emit`
    /// routed to the newly-connected peer -- no daemon restart, no lost
    /// turn, just a redirected sink.
    events_tx: RwLock<mpsc::UnboundedSender<ControlEvent>>,
    /// Path to (or bare name of) the `holo` CLI binary, for `holo stop` (see `stop.rs`).
    holo_bin: String,
    /// `true` while a `Prompt`/`VoiceTranscript` turn is actively running against `client`
    /// (i.e. inside `send_and_stream`). Guards against ever having two simultaneous
    /// `send_and_stream` calls in flight against the same `holo serve`/`AgentApiClient`
    /// session -- see `run_or_queue_prompt`. A `std::sync::Mutex<bool>` rather than an
    /// `AtomicBool` so the "check busy, and if free mark busy" step is one atomic
    /// critical section together with the queue-length check below (a plain
    /// compare-and-swap on its own can't also observe/mutate `queue` in the same step).
    ///
    /// This is also the concrete mechanism that enforces
    /// [`crate::limits::MAX_ACTIVE_TASKS_PER_MAC`] (PRD 10.4): with the cap
    /// at 1, "at most one task actively running" is exactly "at most one
    /// `Prompt`/`VoiceTranscript` turn holds `busy == true` at a time" --
    /// every other concurrent request is queued (see `queue` below), never
    /// run concurrently. No separate counter is needed for a cap of 1; this
    /// doc comment exists so that equivalence is explicit rather than an
    /// unstated coincidence.
    busy: Mutex<bool>,
    /// Prompts that arrived while `busy` was `true`, oldest-first (`pop_front` drains in
    /// arrival order). Guarded by the same lock discipline as `busy`: both are read/written
    /// together under `queue`'s own mutex so "is anything running" and "what's queued" never
    /// observe a torn state relative to each other.
    queue: Mutex<VecDeque<QueuedPrompt>>,
    /// Weak backref to the owning [`super::HoloBridge`], installed by `main.rs` right after
    /// the bridge is `Arc`-wrapped (see [`Self::attach_bridge`]). Weak, not `Arc`: the bridge
    /// OWNS this control bridge, so a strong ref here would be a cycle. Used by the
    /// rate-limit failover in [`Self::run_prompt`] -- backend switching (terminate + respawn
    /// `holo serve`) lives on the bridge, which owns the process slot. Empty (never attached,
    /// or upgrade fails during shutdown) simply disables failover for the turn.
    bridge: std::sync::OnceLock<std::sync::Weak<super::HoloBridge>>,
    /// Per-task PLAN/EXECUTE/VERIFY/DONE phase tracking -- see `crate::task_fsm`'s module doc
    /// for the design rationale (a native reimplementation of `rs-plugkit`'s phase-FSM
    /// pattern, grounded in this daemon's own real A2A `TrajectoryEvent` signal). Owned here
    /// (not a daemon-wide singleton) since every task this bridge runs is already serialized
    /// through `busy`/`queue` above.
    tasks: crate::task_fsm::TaskRegistry,
    /// Environment/user-context memory (see `crate::env_context`'s module doc). `None` when
    /// the store failed to open (e.g. `$HOME` unset in some unusual launch environment) --
    /// degrade-don't-crash, matching every other best-effort subsystem in this bridge; a
    /// missing store just means no context gets prepended, never a turn failure.
    env_context: Option<crate::env_context::EnvContextStore>,
}

/// What one streaming attempt of a turn produced -- see [`HoloControlBridge::run_prompt`]'s
/// retry loop.
enum TurnOutcome {
    /// The turn ran to its natural end (success, cancel, cap, or a failure that was already
    /// emitted to the peer). Nothing further to do.
    Completed,
    /// The turn died with a backend-error shape and its tail events were SUPPRESSED, not
    /// emitted. The caller decides: switch backends and retry, or emit `original` (in
    /// order) after all. TWO real shapes arm this, both probe-witnessed:
    ///
    /// 1. `Failed` terminal matching [`is_backend_error_message`] -- the hosted backend's
    ///    rate-limit 429s reach this bridge as `holo serve`'s generic `"agent backend
    ///    error"` (serve.py swallows the HTTP detail).
    /// 2. `Completed` terminal with NO answer text at all -- when the runtime's model calls
    ///    all error (witnessed against a dead inference endpoint), the runtime "finishes"
    ///    the run anyway and holo serve reports success with an empty answer. The phone
    ///    would see a task that silently did nothing. An armed turn treats that empty
    ///    completion as the failure it is; the retry's own (possibly legitimately empty)
    ///    result is emitted as-is, so a genuinely-empty turn costs one extra attempt, never
    ///    a lost result.
    BackendFailure { original: Vec<ControlEvent> },
}

/// Does this failure text look like the agent backend (rather than this daemon or the A2A
/// transport) died? `"agent backend error"` is `holo serve`'s literal, generic message for
/// ANY failed agent-API call (serve.py wraps `httpx.HTTPError` -- the hosted 429 detail is
/// swallowed before it reaches A2A, so a broader retry-once-on-the-other-backend is the
/// best available response). The 429/rate-limit forms cover transport-level errors that DO
/// carry detail.
fn is_backend_error_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("agent backend error")
        || lower.contains("429")
        || lower.contains("rate limit")
        || lower.contains("rate-limit")
        || lower.contains("too many requests")
}

impl HoloControlBridge {
    pub fn new(
        client: A2aClient,
        holo_bin: impl Into<String>,
        events_tx: mpsc::UnboundedSender<ControlEvent>,
    ) -> Self {
        Self {
            client: RwLock::new(client),
            events_tx: RwLock::new(events_tx),
            holo_bin: holo_bin.into(),
            busy: Mutex::new(false),
            queue: Mutex::new(VecDeque::new()),
            bridge: std::sync::OnceLock::new(),
            tasks: crate::task_fsm::TaskRegistry::new(),
            env_context: match crate::env_context::EnvContextStore::open() {
                Ok(store) => Some(store),
                Err(err) => {
                    tracing::warn!(
                        error = %format!("{err:#}"),
                        "env_context store failed to open; turns will run without environment-context injection"
                    );
                    None
                }
            },
        }
    }

    /// Install the weak backref to the owning bridge (see the `bridge` field). Idempotent:
    /// a second call is ignored (`OnceLock::set`), which cannot happen today -- `main.rs`
    /// attaches exactly once, right after `Arc::new(bridge)`.
    pub fn attach_bridge(&self, bridge: std::sync::Weak<super::HoloBridge>) {
        let _ = self.bridge.set(bridge);
    }

    /// Swap in a freshly-built `A2aClient` pointed at a respawned `holo serve` process. See
    /// `HoloBridge::restart_process`, the only caller. Does not touch `busy`/`queue`/`events_tx`
    /// -- only which process subsequent A2A calls go to.
    pub fn replace_client(&self, client: A2aClient) {
        *self.client.write().expect("client lock poisoned") = client;
    }

    /// Reports whether a turn is currently running and how many more are queued behind it.
    /// Used by `crate::control_channel::ControlChannel::accept` to greet a freshly (re)connected
    /// peer with the daemon's actual in-flight/queue state instead of silence -- relevant after
    /// a reconnect, where a stale in-flight Holo task from before the drop may still be running
    /// (or queued prompts from it may still be waiting) and the newly-connected peer has no
    /// other way to learn that without waiting for the next `ControlEvent`.
    pub fn busy_state(&self) -> (bool, usize) {
        let busy = *self.busy.lock().expect("busy lock poisoned");
        let queued = self.queue.lock().expect("queue lock poisoned").len();
        (busy, queued)
    }

    /// Swaps the sink that [`emit`](Self::emit) sends
    /// [`ControlEvent`]s to. Used when a new control-channel connection is
    /// accepted (see `crate::control_channel::ControlChannel::accept`) so
    /// events from this bridge's in-flight/future turns are routed to the
    /// currently-connected peer rather than a stale, possibly-closed sink
    /// from a previous connection.
    pub fn replace_event_sink(&self, events_tx: mpsc::UnboundedSender<ControlEvent>) {
        *self.events_tx.write().expect("events_tx lock poisoned") = events_tx;
    }

    /// Emit a [`ControlEvent::DaemonStatus`] -- see `holo_bridge::health`, the only caller
    /// outside this module. Public (unlike [`Self::emit`]) since it's the sole way an
    /// out-of-band supervisor (not itself in possession of a `request_id`) can surface
    /// something to the currently-connected peer over the same event path every A2A-derived
    /// event already flows through.
    pub fn emit_daemon_status(&self, text: impl Into<String>) {
        self.emit(ControlEvent::DaemonStatus { text: text.into() });
    }

    fn emit(&self, event: ControlEvent) {
        // A dropped receiver (control channel torn down mid-turn) is not this bridge's
        // problem to escalate -- the in-flight A2A call keeps running to completion
        // server-side regardless, matching holo-desktop-cli's own "cancel is best-effort,
        // local state always resets" stance in session_runner.py.
        let _ = self
            .events_tx
            .read()
            .expect("events_tx lock poisoned")
            .send(event);
    }

    /// Handle one incoming control message end-to-end. Prompts/transcripts stream until the
    /// A2A task reaches a terminal state (or the bridge errors); stop requests return as soon
    /// as the stop/cancel calls are issued (mirrors `holo stop`'s own fire-and-forget
    /// semantics -- see `stop.rs`).
    pub async fn handle(&self, message: ControlMessage) {
        match message {
            ControlMessage::Prompt {
                request_id,
                text,
                context_id,
            }
            | ControlMessage::VoiceTranscript {
                request_id,
                text,
                context_id,
                confidence: _,
            } => {
                self.handle_prompt(request_id, text, context_id).await;
            }
            ControlMessage::Stop {
                request_id,
                context_id,
                force,
            } => {
                self.handle_stop(request_id, context_id, force).await;
            }
        }
    }

    /// Entry point for `Prompt`/`VoiceTranscript`: either runs the turn immediately (no turn
    /// currently in flight) or queues it and returns, per the task's explicit "queue, don't
    /// drop, don't race" requirement. `context_id` is always `None` on the way in today (see
    /// `to_control_message`'s doc), so every queued prompt starts a fresh A2A context in the
    /// order it was received -- exactly the sequencing a single-active-session
    /// `AgentApiClient` requires.
    async fn handle_prompt(&self, request_id: String, text: String, context_id: Option<String>) {
        self.emit(ControlEvent::Ack {
            request_id: request_id.clone(),
        });

        {
            // Single critical section: observe-and-flip `busy` and (if already busy)
            // push onto `queue` atomically, so a second `handle_prompt` call racing in
            // from a different accepted connection can never both see `busy == false`
            // and both proceed to `run_prompt` -- exactly the "two simultaneous
            // requests" race the task calls out.
            let mut busy = self.busy.lock().expect("busy lock poisoned");
            if *busy {
                let mut queue = self.queue.lock().expect("queue lock poisoned");
                let ahead = queue.len();
                queue.push_back(QueuedPrompt {
                    request_id: request_id.clone(),
                    text,
                    context_id,
                });
                drop(queue);
                drop(busy);
                tracing::debug!(request_id, ahead, "prompt queued behind an in-flight turn");
                self.emit(ControlEvent::Queued { request_id, ahead });
                return;
            }
            // `busy` flipping true here is the one and only place a task becomes "active" on
            // this Mac; `MAX_ACTIVE_TASKS_PER_MAC` (PRD 10.4) is a `usize` rather than a `bool`
            // to describe an eventual general cap, but a plain boolean IS the cap==1 case (a
            // `bool` has exactly two states, "0 active" / "1 active", matching a cap of 1
            // exactly) -- this assertion makes that equivalence a real, checked invariant
            // instead of only a doc comment's claim.
            debug_assert_eq!(
                crate::limits::MAX_ACTIVE_TASKS_PER_MAC,
                1,
                "busy: Mutex<bool> only models a max-1-active-task cap; a higher cap needs a counter, not a bool"
            );
            *busy = true;
        }

        self.run_prompt(request_id, text, context_id.as_deref())
            .await;
        self.drain_queue().await;
    }

    /// Runs one turn against `client` end-to-end (streaming progress, terminal
    /// ack/answer/error). Does not touch `busy`/`queue` itself -- callers
    /// (`handle_prompt` for the immediately-run case, `drain_queue` for queued ones) own the
    /// busy-flag lifecycle so this stays a plain "run this one turn" primitive reusable from
    /// both call sites.
    ///
    /// ## Agent action cap ([`crate::limits::AGENT_ACTION_CAP_DEFAULT`], PRD 10.4)
    ///
    /// A fresh [`ActionCounter`] is constructed per turn and
    /// [`ActionCounter::try_record`] is called once for every
    /// [`TaskUpdate::Working`] update the backend streams -- each `Working`
    /// update is one unit of observable agent progress (a tool call/step),
    /// which is the closest real signal this bridge has to "one agent
    /// action" (the backend's `TrajectoryEvent` union is opaque JSON to
    /// this crate -- see `holo_bridge`'s own module doc on why -- so a
    /// finer per-tool-call count isn't available without decoding it).
    ///
    /// **Real enforcement, with one honestly-documented limitation**: once
    /// the cap is hit, every subsequent update for this turn -- of *any*
    /// variant, not just further `Working` ones -- is suppressed via a
    /// latch (`capped`), and the turn's outcome is reported as a capped
    /// [`ControlEvent::Error`] instead of whatever the backend eventually
    /// returns. The latch matters: without it, an `Answer`/`Terminal`
    /// update arriving right after the capped `Working` update would skip
    /// the action-counting branch entirely (it isn't `Working`) and get
    /// emitted immediately, racing ahead of (or duplicating) the
    /// capped-turn error emitted once `send_and_stream` returns. What this
    /// does **not** do: actually stop `holo serve` from continuing to run
    /// the agent past the 100th action server-side.
    /// [`crate::holo_bridge::a2a_client::A2aClient::send_and_stream`]'s
    /// `on_update` callback is a plain synchronous `FnMut(TaskUpdate)` with
    /// no return value the caller can use to signal "abort the stream", and
    /// issuing a real `tasks/cancel` requires the resolved `context_id`
    /// [`crate::holo_bridge::a2a_client::A2aClient::send_and_stream`] only
    /// returns *after* the stream ends -- so a true server-side abort
    /// exactly at action 101 is not reachable without changing
    /// `send_and_stream`'s callback contract to let it return an abort
    /// signal, which is out of this change's scope (see this module's
    /// `holo_bridge` doc-level "what could not be confirmed" convention:
    /// this is the same kind of honestly-scoped gap, not a silent one).
    async fn run_prompt(&self, request_id: String, text: String, context_id: Option<&str>) {
        // Phase-FSM lifecycle: begin tracking once per TASK (not per failover-retry attempt --
        // a retry is still the same task continuing, so `run_prompt_once` observations across
        // attempts accumulate onto the one FSM `begin` creates here), conclude on every exit
        // path via the guard below. See `crate::task_fsm`'s module doc.
        self.tasks.begin(&request_id);
        struct ConcludeOnDrop<'a> {
            tasks: &'a crate::task_fsm::TaskRegistry,
            request_id: &'a str,
        }
        impl Drop for ConcludeOnDrop<'_> {
            fn drop(&mut self) {
                self.tasks.conclude(self.request_id);
            }
        }
        let _conclude_task = ConcludeOnDrop { tasks: &self.tasks, request_id: &request_id };

        // Failover pre-step: if the fallback backend has outlived its cooldown, hop back to
        // the primary so the hosted path gets probed again (its rate limit has likely reset).
        let bridge = self.bridge.get().and_then(std::sync::Weak::upgrade);
        if let Some(bridge) = &bridge {
            if bridge.maybe_restore_primary().await {
                self.emit_daemon_status(
                    "fallback cooldown elapsed; inference is back on the primary backend",
                );
            }
        }

        // Intent-routing pre-step: pick the right holo3 model for this prompt's apparent
        // complexity (see `crate::router`'s module doc). A no-op in local (no-cloud) mode or
        // while on the tinfoil fallback -- `HoloBridge::route_model` enforces both. Best-effort:
        // a failed switch is logged and the turn proceeds on whatever model is already active
        // (degrade-don't-crash, matching every other backend-swap call site in this bridge).
        if let Some(bridge) = &bridge {
            if let Err(err) = bridge.route_model(&text).await {
                tracing::warn!(
                    request_id,
                    error = %format!("{err:#}"),
                    "intent router model switch failed; continuing on the currently active model"
                );
            }
        }

        // Environment-context injection: prepend the most relevant durable facts about THIS
        // user's setup (see `crate::env_context`'s module doc -- the concrete motivating bug
        // was the agent opening a new terminal instead of finding the existing Ghostty
        // session). Runs AFTER route_model intentionally: the router's complexity classifier
        // should score the user's actual words, not text padded with a context block, which
        // could skew the heuristic toward "complex" on every turn. Best-effort: retrieval
        // failure (model not yet cached, embedding error) logs and falls through to the
        // unmodified prompt -- a missing context block should never fail a turn.
        let augmented_text = match &self.env_context {
            Some(store) => match store.retrieve(&text, 5).await {
                Ok(facts) => match crate::env_context::format_context_block(&facts) {
                    Some(block) => format!("{block}\n{text}"),
                    None => text.clone(),
                },
                Err(err) => {
                    tracing::debug!(
                        request_id,
                        error = %format!("{err:#}"),
                        "env_context retrieval failed; sending prompt without environment context"
                    );
                    text.clone()
                }
            },
            None => text.clone(),
        };

        // At most ONE failover retry per turn: attempt 0 may suppress a backend failure and
        // switch to the fallback; attempt 1 runs with failover disabled, so its failure --
        // whatever the shape -- is emitted for real. No unbounded retry loops.
        let mut attempt = 0u32;
        loop {
            let failover_armed = attempt == 0 && bridge.is_some();
            match self
                .run_prompt_once(&request_id, &augmented_text, context_id, failover_armed)
                .await
            {
                TurnOutcome::Completed => return,
                TurnOutcome::BackendFailure { original } => {
                    let Some(bridge) = &bridge else {
                        for event in original {
                            self.emit(event);
                        }
                        return;
                    };
                    match bridge.activate_fallback().await {
                        Ok(super::FallbackActivation::Switched { label }) => {
                            tracing::warn!(
                                request_id,
                                fallback = %label,
                                "agent backend failed (rate-limit shape); switched to fallback backend, retrying turn"
                            );
                            self.emit(ControlEvent::Progress {
                                request_id: request_id.clone(),
                                context_id: None,
                                text: Some(format!(
                                    "agent backend unavailable (likely rate-limited) -- retrying via fallback model {label}"
                                )),
                                raw_event: None,
                            });
                            attempt += 1;
                        }
                        Ok(super::FallbackActivation::AlreadyActive
                        | super::FallbackActivation::Unavailable) => {
                            for event in original {
                                self.emit(event);
                            }
                            return;
                        }
                        Err(err) => {
                            tracing::warn!(
                                request_id,
                                error = %format!("{err:#}"),
                                "fallback backend switch failed; surfacing the original turn failure"
                            );
                            for event in original {
                                self.emit(event);
                            }
                            return;
                        }
                    }
                }
            }
        }
    }

    /// One streaming attempt of a turn. With `failover_armed`, a terminal `Failed` whose
    /// message matches [`is_backend_error_message`] is captured and returned as
    /// [`TurnOutcome::BackendFailure`] INSTEAD of being emitted -- the caller either retries
    /// on the fallback backend (peer sees a Progress note, then the retry's own events; no
    /// spurious failure) or, if it can't switch, emits the captured event unchanged.
    async fn run_prompt_once(
        &self,
        request_id: &str,
        text: &str,
        context_id: Option<&str>,
        failover_armed: bool,
    ) -> TurnOutcome {
        let request_id = request_id.to_owned();
        let request_id_for_updates = request_id.clone();
        // The suppressed backend-failure tail (terminal, and the empty Answer preceding it
        // in the silent-failure shape), if any -- see the callback below. A mutex (not a
        // Cell) only because the callback is `Fn`-shaped from the borrow checker's
        // perspective across the stream; contention is impossible (updates are serial).
        let suppressed: std::sync::Mutex<Vec<ControlEvent>> = std::sync::Mutex::new(Vec::new());
        // Whether this turn produced a real (non-empty) answer -- the discriminator between
        // a legitimate completion and the silent-failure shape (see TurnOutcome docs).
        let saw_real_answer = std::sync::atomic::AtomicBool::new(false);
        // Whether ANY terminal update arrived -- decides whether a hit action cap is
        // reported as a log-only note (turn still concluded properly) or a real error.
        let saw_terminal = std::sync::atomic::AtomicBool::new(false);
        let actions = ActionCounter::new_default();
        // Latched once the cap is hit, so every subsequent update of *any* variant -- not just
        // further `Working` ones -- is suppressed too. Without this, an `Answer`/`Terminal`
        // update arriving right after the capped `Working` update would skip the
        // `try_record`-guarded branch entirely (it isn't `Working`) and get emitted immediately,
        // racing ahead of the capped-turn `ControlEvent::Error` this function emits below --
        // the peer could see a real success/terminal event interleaved with (or before) the
        // cap error. A `std::sync::atomic::AtomicBool` rather than reusing `actions.count()`
        // for this check because `count() >= cap()` is also true exactly on the call that
        // *causes* the cap to be hit, and that call's own `Working` update must still be
        // suppressed by the `try_record` branch above it, not one this flag would also catch
        // one update later than necessary.
        let capped = std::sync::atomic::AtomicBool::new(false);
        let client = self.client.read().expect("client lock poisoned").clone();
        let result = client
            .send_and_stream(&text, context_id, |update| {
                // Cap latch: once hit, further WORKING updates are suppressed -- but
                // Answer/Terminal updates still flow. The original everything-latch was
                // live-witnessed turning a SUCCEEDING turn into a cap error: an 11-minute
                // kimi-k2-6 turn latched at update #100, and its real final answer
                // (produced 8 minutes later) was swallowed with the rest.
                if capped.load(Ordering::SeqCst)
                    && matches!(update, TaskUpdate::Working { .. })
                {
                    return;
                }
                if matches!(update, TaskUpdate::Terminal { .. }) {
                    saw_terminal.store(true, Ordering::SeqCst);
                }
                // Phase-FSM observation: fed BEFORE the cap/failover suppression logic below
                // decides what reaches the phone, so phase tracking reflects what the backend
                // actually did on this attempt, not what got shown. A phase change is folded
                // into a Progress status line for the CURRENT emit below (not a separate
                // event) rather than re-plumbing a new ControlEvent variant through
                // `control_channel`'s wire mapping -- see `phase_status_text`'s doc.
                let phase_change_text: Option<String> = self.tasks.with_task(&request_id_for_updates, |fsm| {
                    let changed = match &update {
                        TaskUpdate::Working { raw_event, .. } => fsm.observe_working(raw_event.as_ref()),
                        TaskUpdate::Answer { text } => fsm.observe_answer(text),
                        TaskUpdate::Terminal { state, .. } => {
                            fsm.advance_terminal(*state);
                            false
                        }
                    };
                    changed.then(|| fsm.phase_status_text())
                }).flatten();
                if let Some(text) = phase_change_text {
                    self.emit(ControlEvent::DaemonStatus { text });
                }
                if matches!(update, TaskUpdate::Working { .. }) {
                    if let Err(cap) = actions.try_record() {
                        tracing::warn!(
                            request_id = request_id_for_updates,
                            cap,
                            "agent action cap reached for this task (PRD 10.4); suppressing further progress events"
                        );
                        capped.store(true, Ordering::SeqCst);
                        return;
                    }
                }
                // Failover interception (armed turns only): capture instead of emitting --
                // if the switch succeeds, the peer never sees a failure/no-op for a turn
                // that then succeeds on the fallback; if it can't switch, the caller emits
                // these exact events in order, so nothing is ever lost.
                if failover_armed {
                    match &update {
                        // Shape 1: explicit backend-error failure.
                        TaskUpdate::Terminal {
                            state: TerminalState::Failed,
                            message: Some(message),
                        } if is_backend_error_message(message) => {
                            suppressed
                                .lock()
                                .expect("suppressed lock poisoned")
                                .push(translate_update(&request_id_for_updates, update));
                            return;
                        }
                        // Shape 2 groundwork: hold back an EMPTY answer -- it only becomes
                        // a failure if the terminal confirms nothing else happened.
                        TaskUpdate::Answer { text } if text.trim().is_empty() => {
                            suppressed
                                .lock()
                                .expect("suppressed lock poisoned")
                                .push(translate_update(&request_id_for_updates, update));
                            return;
                        }
                        TaskUpdate::Answer { .. } => {
                            saw_real_answer.store(true, Ordering::SeqCst);
                        }
                        // Shape 2: Completed with no real answer = the silent-failure
                        // completion (runtime errored every model call, "finished" anyway).
                        TaskUpdate::Terminal {
                            state: TerminalState::Completed,
                            ..
                        } if !saw_real_answer.load(Ordering::SeqCst) => {
                            suppressed
                                .lock()
                                .expect("suppressed lock poisoned")
                                .push(translate_update(&request_id_for_updates, update));
                            return;
                        }
                        _ => {}
                    }
                }
                self.emit(translate_update(&request_id_for_updates, update));
            })
            .await;

        if actions.count() >= actions.cap() {
            tracing::warn!(
                request_id,
                actions = actions.count(),
                cap = actions.cap(),
                "prompt turn hit the agent action cap (PRD 10.4); progress was suppressed but the turn's answer/terminal still flowed"
            );
            // The turn's own Answer/Terminal (if any) already flowed through the latch
            // above. When a terminal DID arrive the turn concluded properly, so this is
            // a log-only status note -- an Error event here would let the phone re-fail
            // a task that just succeeded. Only a stream that never reached a terminal
            // gets the cap as its real error.
            let note = format!(
                "agent action cap reached ({} actions, cap {}); intermediate progress was suppressed \
                 from that point. This does not stop the backend agent server-side -- see \
                 HoloControlBridge::run_prompt's doc.",
                actions.count(),
                actions.cap()
            );
            if saw_terminal.load(Ordering::SeqCst) {
                self.emit(ControlEvent::DaemonStatus { text: note });
            } else {
                self.emit(ControlEvent::Error {
                    request_id: request_id.clone(),
                    message: note,
                });
            }
        }

        match result {
            Ok(resolved_context_id) => {
                let held = std::mem::take(
                    &mut *suppressed.lock().expect("suppressed lock poisoned"),
                );
                // Held events only count as a backend failure when a TERMINAL is among
                // them (an empty Answer alone followed by a real Failed took shape 1's
                // path; an empty Answer with no terminal at all means the stream ended
                // oddly -- emit what was held and let the transport error path speak).
                let has_terminal = held
                    .iter()
                    .any(|e| matches!(e, ControlEvent::Done { .. }));
                if has_terminal {
                    return TurnOutcome::BackendFailure { original: held };
                }
                for event in held {
                    self.emit(event);
                }
                tracing::debug!(
                    request_id,
                    context_id = %resolved_context_id,
                    "prompt turn finished"
                );
                TurnOutcome::Completed
            }
            Err(err) => {
                let message = err.to_string();
                if failover_armed && is_backend_error_message(&message) {
                    // Transport-level rate-limit shape (these DO carry the HTTP detail,
                    // unlike serve.py's swallowed terminal): same failover treatment.
                    return TurnOutcome::BackendFailure {
                        original: vec![ControlEvent::Error {
                            request_id,
                            message,
                        }],
                    };
                }
                tracing::warn!(request_id, error = %err, "prompt turn failed before a terminal A2A state");
                self.tasks.with_task(&request_id, |fsm| fsm.fail());
                self.emit(ControlEvent::Error {
                    request_id,
                    message,
                });
                TurnOutcome::Completed
            }
        }
    }

    /// After a turn finishes, pop and run the next queued prompt (if any), looping until the
    /// queue is empty or a `Stop` (running concurrently under the same locks) drains it out
    /// from under this loop. Re-emits `Queued { ahead: 0 }`-implying progress by simply
    /// starting the next turn's own `Ack`/`Progress`/... events -- the queued prompts behind
    /// it, if any, keep whatever `ahead` count they were given when they were first queued;
    /// re-announcing a shrinking count on every drain step is not required by the task's
    /// acceptance (one queued-status message per queued prompt) and would just be additional
    /// noise on every drain step for long queues.
    async fn drain_queue(&self) {
        loop {
            let next = {
                let mut queue = self.queue.lock().expect("queue lock poisoned");
                let next = queue.pop_front();
                let mut busy = self.busy.lock().expect("busy lock poisoned");
                *busy = next.is_some();
                next
            };
            let Some(QueuedPrompt {
                request_id,
                text,
                context_id,
            }) = next
            else {
                break;
            };
            self.run_prompt(request_id, text, context_id.as_deref())
                .await;
        }
    }

    async fn handle_stop(&self, request_id: String, context_id: Option<String>, force: bool) {
        self.emit(ControlEvent::Ack {
            request_id: request_id.clone(),
        });

        // `holo stop` (below) only reaches the *currently running* turn -- it polls
        // `~/.holo/stop` from inside `session_runner.run_turn`, which a still-queued prompt
        // has not entered yet (see stop.rs's module doc). Left alone, queued prompts would
        // survive a Stop and get dispatched to `holo serve` right after, silently ignoring
        // the user's stop. Drain the queue here and give each dropped prompt its own terminal
        // `Done{Canceled}` event instead of leaving its `request_id` unresolved forever on the
        // iOS side.
        let dropped: Vec<QueuedPrompt> = {
            let mut queue = self.queue.lock().expect("queue lock poisoned");
            queue.drain(..).collect()
        };
        for dropped_prompt in dropped {
            tracing::debug!(
                request_id = dropped_prompt.request_id,
                "queued prompt dropped by stop"
            );
            self.emit(ControlEvent::Done {
                request_id: dropped_prompt.request_id,
                context_id: dropped_prompt.context_id,
                status: DoneStatus::Canceled,
                message: Some("canceled: stop requested while queued".to_owned()),
            });
        }

        // Scoped cancel first, when we have a context to scope it to: the A2A
        // `tasks/cancel`-equivalent path (HoloExecutor.cancel -> best-effort backend
        // session cancel; see a2a_client module doc). This is the lower-blast-radius option
        // and should win when both are meaningful.
        if let Some(ctx) = context_id.as_deref() {
            let client = self.client.read().expect("client lock poisoned").clone();
            if let Err(err) = client.cancel(ctx, None).await {
                tracing::warn!(request_id, context_id = ctx, error = %err, "A2A tasks/cancel failed");
                self.emit(ControlEvent::Error {
                    request_id: request_id.clone(),
                    message: format!("A2A cancel failed for context {ctx}: {err}"),
                });
            }
        }

        // Global `holo stop` kill switch: always issued when no context_id was given (the
        // caller wants "stop whatever is running"), or when `force` was requested (force
        // implies a process-level SIGKILL that only the CLI path performs -- see stop.rs).
        if context_id.is_none() || force {
            if let Err(err) = crate::holo_bridge::stop::holo_stop(&self.holo_bin, force).await {
                tracing::warn!(request_id, error = %err, "holo stop failed");
                self.emit(ControlEvent::Error {
                    request_id: request_id.clone(),
                    message: format!("holo stop failed: {err}"),
                });
                return;
            }
        }

        self.emit(ControlEvent::Done {
            request_id,
            context_id,
            status: DoneStatus::Canceled,
            message: Some(if force {
                "stop requested (forced)".to_owned()
            } else {
                "stop requested".to_owned()
            }),
        });
    }
}

fn translate_update(request_id: &str, update: TaskUpdate) -> ControlEvent {
    match update {
        TaskUpdate::Working { raw_event, text } => ControlEvent::Progress {
            request_id: request_id.to_owned(),
            context_id: None,
            text,
            raw_event,
        },
        TaskUpdate::Answer { text } => ControlEvent::Answer {
            request_id: request_id.to_owned(),
            context_id: None,
            text,
        },
        TaskUpdate::Terminal { state, message } => ControlEvent::Done {
            request_id: request_id.to_owned(),
            context_id: None,
            status: state.into(),
            message,
        },
    }
}
