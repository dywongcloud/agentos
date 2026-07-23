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
use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::mpsc;

use holoiroh_wire::{ClarifyingQuestion, InputRequestKind, MouseButton, RemoteControlEvent};

use crate::holo_bridge::a2a_client::{A2aClient, TaskUpdate, TerminalState};
use crate::limits::ActionCounter;
use crate::sensitive_categories::{CategorySetting, SensitiveCategories};

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
    /// Pause the in-flight turn: scoped-cancel it (the backend exposes no real pause RPC --
    /// see `ClientMessage::Pause`'s doc in `holoiroh-wire`) while stashing its instruction
    /// text and resolved `contextId` so a later `Resume` continues the same backend session.
    Pause { request_id: String },
    /// Resume the turn a previous `Pause` (or a sensitive-app consent gate) stashed.
    Resume { request_id: String },
    /// Replace whatever is running/queued with `text`: cancel the in-flight turn, drop the
    /// queue, then run `text` -- reusing the canceled turn's `contextId` when known so the
    /// agent keeps the history it had built up.
    Redirect { request_id: String, text: String },
    /// A hands-on remote-control action the user performed by touching the iOS
    /// live-share view -- an escalation to direct control. `TakeControl`/
    /// `ReleaseControl` pause/resume the agent; the other actions are injected
    /// as real CGEvents on the Mac (see `crate::remote_input`).
    RemoteControl { event: RemoteControlEvent },
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
            | ControlMessage::Stop { request_id, .. }
            | ControlMessage::Pause { request_id }
            | ControlMessage::Resume { request_id }
            | ControlMessage::Redirect { request_id, .. } => request_id,
            // Remote-control input events are not correlated to a turn/request.
            ControlMessage::RemoteControl { .. } => "",
        }
    }
}

/// Inject a non-take/release remote-control action as real CGEvents on the Mac
/// (see `crate::remote_input`). `TakeControl`/`ReleaseControl` are handled by
/// the caller (they pause/resume the agent, not inject input).
fn inject_remote_input(event: RemoteControlEvent) {
    match event {
        RemoteControlEvent::Move { x, y } => crate::remote_input::move_cursor(x, y),
        RemoteControlEvent::Button { x, y, button, down } => {
            crate::remote_input::button(x, y, matches!(button, MouseButton::Right), down)
        }
        RemoteControlEvent::Click { x, y, button, count } => {
            crate::remote_input::click(x, y, matches!(button, MouseButton::Right), count)
        }
        RemoteControlEvent::Scroll { x, y, dx, dy } => crate::remote_input::scroll(x, y, dx, dy),
        RemoteControlEvent::Text { text } => crate::remote_input::text(&text),
        RemoteControlEvent::Key { key, down } => crate::remote_input::key(&key, down),
        RemoteControlEvent::TakeControl | RemoteControlEvent::ReleaseControl => {}
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
    /// Live task-control state change, not tied to a single request: emitted by
    /// cooperative auto-yield (`crate::auto_yield`) when it steps the agent
    /// aside (`paused: true`) or resumes it (`paused: false`), so the phone's
    /// Pause/Stop pill reflects the yield in real time. Maps to the wire
    /// `ServerMessage::TaskActive` (the same message used on reconnect).
    TaskActive { paused: bool, queued: usize },
    /// The daemon needs structured user input before the paused turn can continue --
    /// today produced only by the sensitive-app consent gate (see the sensitive-app
    /// watchdog in this module). Translated by `control_channel::from_control_event` into
    /// the wire `ServerMessage::InputRequest` (P0-14). `request_id` here is the CONSENT
    /// request's own id (echoed back by `ClientMessage::InputResponse`), distinct from the
    /// paused task's id, which the pending-consent state tracks internally.
    InputRequested {
        request_id: String,
        kind: InputRequestKind,
        context: String,
        response_options: Vec<String>,
        expires_at: u64,
    },
    /// Clarifying questions generated for a `ClientMessage::ClarifyRequest`
    /// (empty when the instruction was already clear). Not tied to a task turn;
    /// `control_channel::from_control_event` maps it to the wire
    /// `ServerMessage::ClarifyQuestions`. Emitted by the control-channel read
    /// loop's spawned clarify task, off the desktop-task pipeline.
    ClarifyQuestions { questions: Vec<ClarifyingQuestion> },
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

/// The turn currently inside `run_prompt`, tracked so Stop/Pause/Redirect (and the
/// sensitive-app watchdog) can scope-cancel it mid-stream. `text` is the ORIGINAL
/// instruction (pre-env-context augmentation) so a pause stash can honestly replay it;
/// `context_id` starts as whatever the caller passed and is upgraded to the stream's
/// resolved `contextId` the moment `send_and_stream`'s `on_context` fires.
#[derive(Clone)]
struct CurrentTurn {
    request_id: String,
    text: String,
    context_id: Option<String>,
    /// The A2A `Task.id` the stream resolved for this turn -- the id
    /// `tasks/cancel` actually requires (a context-id stand-in returns
    /// "Task not found" on the current holo serve; see `A2aClient::cancel`).
    a2a_task_id: Option<String>,
}

/// A turn parked by `Pause` (or by the sensitive-app consent gate): everything `Resume`
/// needs to continue it -- the original instruction plus the backend session (`contextId`)
/// whose history carries the task's progress so far.
#[derive(Clone)]
struct PausedTurn {
    request_id: String,
    text: String,
    context_id: Option<String>,
    /// True if this pause was created by cooperative auto-yield (the user
    /// started using the Mac) rather than a deliberate user Pause. Only an
    /// auto pause is auto-resumed; a user pause stays paused until the user
    /// resumes it. See `crate::auto_yield`.
    auto: bool,
}

/// One outstanding sensitive-app consent request (`ControlEvent::InputRequested`,
/// kind `sensitive_access_consent`), resolved by a matching
/// `ClientMessage::InputResponse` (see [`HoloControlBridge::resolve_consent`]) or by the
/// expiry timer spawned alongside it.
struct PendingConsent {
    consent_request_id: String,
    category_id: String,
}

/// Consent response option meaning "let the agent continue in this app category for the
/// rest of the current task". Shared between the request's `response_options` and
/// `resolve_consent`'s match so the two can never drift apart.
const CONSENT_ALLOW_ONCE: &str = "Allow once";
/// Consent response option meaning "abandon the paused task".
const CONSENT_STOP_TASK: &str = "Stop task";
/// How long a sensitive-app consent request stays answerable before it expires into the
/// standard safe-pause state (the task simply stays paused; `Resume` re-asks).
const CONSENT_TTL: std::time::Duration = std::time::Duration::from_secs(120);

/// See `crate::holo_bridge::stall_watchdog`'s module doc for the full design. How long a task
/// may show zero real phase advancement (`TaskFsm::updated_at_ms` unchanged) while still
/// `Working` before the watchdog nudges it. Conservative on purpose: a genuinely hard step
/// (a slow page load, a long download) must never be mistaken for a stuck agent.
const STALL_WATCHDOG_WINDOW: std::time::Duration = std::time::Duration::from_secs(45);
/// Minimum gap between two nudges for the SAME task, so a still-stuck task after one nudge
/// gets real time to act on it before a second nudge piles on.
const STALL_WATCHDOG_NUDGE_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(60);
/// The autonomous self-correction nudge sent (as a redirect on the SAME backend session) when
/// the watchdog detects a stall. Deliberately echoes `agent_guidance::task_framing_block`'s
/// self-correction rule so the reinforcement arrives exactly when it's needed.
const STALL_WATCHDOG_NUDGE_TEXT: &str = "You have not made visible progress for a while. Before continuing: check whether your LAST action actually did what you intended (text in the wrong field, wrong element clicked, an unexpected dialog or state). If it did not, fix just that one step -- do not restart the whole task -- then continue toward the original goal.";
/// Stable substring of the daemon-status line emitted right before a watchdog nudge, so a
/// probe (and the iOS log panel) can distinguish a self-correction nudge from ordinary status
/// text.
const STALL_WATCHDOG_STATUS_MARKER: &str = "self-correction check:";

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
    env_context: Option<Arc<crate::env_context::EnvContextStore>>,
    /// The turn currently inside [`Self::run_prompt`], if any -- set on entry, `context_id`
    /// upgraded mid-stream via `send_and_stream`'s `on_context`, cleared by the same guard
    /// that concludes the task FSM. This is what Stop/Pause/Redirect and the sensitive-app
    /// watchdog scope their cancels to.
    current_turn: Mutex<Option<CurrentTurn>>,
    /// The turn parked by `Pause`/consent-gate, waiting for `Resume` (or discarded by
    /// `Stop`/`Redirect`). At most one -- pausing while paused is a polite no-op.
    paused: Mutex<Option<PausedTurn>>,
    /// True while the user has escalated to hands-on remote control (between a
    /// `RemoteControl::TakeControl` and its `ReleaseControl`). Cooperative
    /// auto-yield stands down while this is set, so the two don't race over the
    /// pause slot -- the user is deliberately driving.
    remote_control_active: AtomicBool,
    /// PRD section-9 class-5 sensitive-app categories, loaded once from
    /// `~/.holoiroh/sensitive_categories.toml` (seeded with defaults on first run). This is
    /// the live wiring the `sensitive_categories` module's own doc used to disclaim -- the
    /// per-turn watchdog consults it against the REAL frontmost app while the agent acts.
    sensitive_categories: Mutex<SensitiveCategories>,
    /// Category ids the user has consented to ("Allow once") for the CURRENT turn only;
    /// cleared every time a fresh turn starts so consent never silently outlives the task
    /// it was granted for.
    turn_allowances: Mutex<HashSet<String>>,
    /// The outstanding sensitive-app consent request, if any.
    pending_consent: Mutex<Option<PendingConsent>>,
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
                Ok(store) => {
                    let store = Arc::new(store);
                    // Seed the built-in environment facts (Ghostty default terminal, never
                    // interrupt Claude Code, this project) on startup so the soft semantic
                    // layer always has them, not just after a manual seeding step. Spawned so
                    // the first-run embedding-model download never blocks bridge construction;
                    // idempotent upsert-by-key, so re-seeding every start is cheap.
                    let seed_store = store.clone();
                    tokio::spawn(async move {
                        match seed_store.seed_defaults().await {
                            Ok(n) => tracing::info!(facts = n, "env_context: seeded default environment facts"),
                            Err(err) => tracing::warn!(
                                error = %format!("{err:#}"),
                                "env_context: seeding default facts failed (the hard process-awareness guard still carries the same rules)"
                            ),
                        }
                    });
                    Some(store)
                }
                Err(err) => {
                    tracing::warn!(
                        error = %format!("{err:#}"),
                        "env_context store failed to open; turns will run without environment-context injection"
                    );
                    None
                }
            },
            current_turn: Mutex::new(None),
            paused: Mutex::new(None),
            remote_control_active: AtomicBool::new(false),
            sensitive_categories: Mutex::new(
                match SensitiveCategories::load_or_init_default() {
                    Ok(cats) => {
                        tracing::info!(
                            categories = cats.categories.len(),
                            "sensitive-app categories loaded (privacy layer live)"
                        );
                        cats
                    }
                    Err(err) => {
                        tracing::warn!(
                            error = %format!("{err:#}"),
                            "failed to load sensitive-app categories config; using built-in defaults for this run"
                        );
                        SensitiveCategories::default_categories()
                    }
                },
            ),
            turn_allowances: Mutex::new(HashSet::new()),
            pending_consent: Mutex::new(None),
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

    /// Whether a task is currently PARKED (paused) awaiting `Resume`. A parked
    /// turn is not `busy` (it was canceled on the backend when paused -- see
    /// `handle_pause`), so `busy_state` alone reports `false` for it; the
    /// reconnect-visibility path in `crate::control_channel` checks this too so
    /// a paused task from before a drop still restores the client's Pause/Stop
    /// pill (in its Paused state) rather than vanishing.
    pub fn is_paused(&self) -> bool {
        self.paused.lock().expect("paused lock poisoned").is_some()
    }

    /// Whether the current pause was created by cooperative auto-yield (vs a
    /// deliberate user Pause). `crate::auto_yield`'s monitor uses this to know
    /// which pauses it may auto-resume, and to avoid overriding a user pause.
    pub fn is_auto_yielded(&self) -> bool {
        self.paused
            .lock()
            .expect("paused lock poisoned")
            .as_ref()
            .is_some_and(|p| p.auto)
    }

    /// True while the user is in hands-on remote control (see
    /// `handle_remote_control`); cooperative auto-yield stands down while set.
    pub fn is_remote_control_active(&self) -> bool {
        self.remote_control_active.load(Ordering::SeqCst)
    }

    /// Auto-yield: step the running turn aside because the user is actively
    /// using the Mac. Like [`handle_pause`](Self::handle_pause) but tagged
    /// `auto` (so only [`auto_yield_resume`](Self::auto_yield_resume) brings it
    /// back) and it emits `TaskActive{paused:true}` so the phone's pill reflects
    /// the yield live. No-op if nothing is running or something is already
    /// paused (never double-parks, never fights an existing user pause).
    pub async fn auto_yield_pause(&self) {
        if self.paused.lock().expect("paused lock poisoned").is_some() {
            return;
        }
        let current = self
            .current_turn
            .lock()
            .expect("current_turn lock poisoned")
            .clone();
        let Some(turn) = current else { return };
        *self.paused.lock().expect("paused lock poisoned") = Some(PausedTurn {
            request_id: turn.request_id.clone(),
            text: turn.text.clone(),
            context_id: turn.context_id.clone(),
            auto: true,
        });
        self.cancel_current_turn("auto-yield").await;
        let queued = self.queue.lock().expect("queue lock poisoned").len();
        self.emit(ControlEvent::TaskActive { paused: true, queued });
        self.emit_daemon_status(
            "stepping aside while you use the Mac -- I'll resume when you're idle",
        );
    }

    /// Auto-yield: the user has gone idle, so resume the auto-parked turn on its
    /// original backend session (`context_id` preserved -> history intact).
    /// Resumes ONLY an auto-yield pause (a user pause stays paused). Adds a
    /// "continue without repeating completed steps" note to blunt duplicate
    /// side-effects, and emits `TaskActive{paused:false}` so the pill flips back.
    pub async fn auto_yield_resume(&self) {
        // Take the parked turn only if it is an auto-yield pause.
        let parked = {
            let mut guard = self.paused.lock().expect("paused lock poisoned");
            match guard.as_ref() {
                Some(p) if p.auto => guard.take(),
                _ => None,
            }
        };
        let Some(parked) = parked else { return };
        let queued = self.queue.lock().expect("queue lock poisoned").len();
        self.emit(ControlEvent::TaskActive { paused: false, queued });
        self.emit_daemon_status("you're idle again -- resuming the task");
        let request_id = uuid::Uuid::new_v4().to_string();
        self.redispatch_parked(
            request_id,
            parked.text,
            parked.context_id,
            "You were interrupted mid-task because the user started using the Mac, and are now \
             resuming on the SAME session. Look at the current on-screen state and CONTINUE the \
             task from where you left off -- do NOT repeat any step you already completed.",
        )
        .await;
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
            ControlMessage::Pause { request_id } => {
                self.handle_pause(request_id).await;
            }
            ControlMessage::Resume { request_id } => {
                self.handle_resume(request_id).await;
            }
            ControlMessage::Redirect { request_id, text } => {
                self.handle_redirect(request_id, text).await;
            }
            ControlMessage::RemoteControl { event } => {
                self.handle_remote_control(event).await;
            }
        }
    }

    /// Route a remote-control action from the iOS live-share view: `TakeControl`
    /// pauses the agent for the duration (a USER pause -- cooperative auto-yield
    /// won't auto-resume it -- so the agent doesn't fight the user's hands-on
    /// control); `ReleaseControl` resumes it; every other action is injected as a
    /// real CGEvent on the Mac via `crate::remote_input`, gated on Accessibility.
    async fn handle_remote_control(&self, event: RemoteControlEvent) {
        match event {
            RemoteControlEvent::TakeControl => {
                // Cooperative auto-yield stands down while the user is driving,
                // so the two don't race over the pause slot.
                self.remote_control_active.store(true, Ordering::SeqCst);
                // If auto-yield already parked the turn, convert it to a USER
                // pause so auto-yield can't resume it mid-control.
                if let Some(p) = self.paused.lock().expect("paused lock poisoned").as_mut() {
                    p.auto = false;
                }
                // Otherwise, park the running turn now.
                if self.paused.lock().expect("paused lock poisoned").is_none() {
                    let current = self
                        .current_turn
                        .lock()
                        .expect("current_turn lock poisoned")
                        .clone();
                    if let Some(turn) = current {
                        *self.paused.lock().expect("paused lock poisoned") = Some(PausedTurn {
                            request_id: turn.request_id.clone(),
                            text: turn.text.clone(),
                            context_id: turn.context_id.clone(),
                            auto: false,
                        });
                        self.cancel_current_turn("remote control").await;
                    }
                }
                if self.paused.lock().expect("paused lock poisoned").is_some() {
                    let queued = self.queue.lock().expect("queue lock poisoned").len();
                    self.emit(ControlEvent::TaskActive { paused: true, queued });
                }
                self.emit_daemon_status("you took control -- the agent is paused while you drive");
            }
            RemoteControlEvent::ReleaseControl => {
                self.remote_control_active.store(false, Ordering::SeqCst);
                let parked = self.paused.lock().expect("paused lock poisoned").take();
                if let Some(parked) = parked {
                    let queued = self.queue.lock().expect("queue lock poisoned").len();
                    self.emit(ControlEvent::TaskActive { paused: false, queued });
                    self.emit_daemon_status("you released control -- resuming the agent");
                    let request_id = uuid::Uuid::new_v4().to_string();
                    self.redispatch_parked(
                        request_id,
                        parked.text,
                        parked.context_id,
                        "The user took manual control and has now released it; resume on the SAME \
                         session and continue where you left off without repeating completed steps.",
                    )
                    .await;
                } else {
                    self.emit_daemon_status("you released control");
                }
            }
            input => {
                if crate::remote_input::is_permitted() {
                    inject_remote_input(input);
                } else {
                    // Emit the Accessibility grant hint once, so a missing grant
                    // is actionable rather than a silent no-op.
                    static WARNED: std::sync::atomic::AtomicBool =
                        std::sync::atomic::AtomicBool::new(false);
                    if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                        self.emit_daemon_status(
                            "to control the Mac from your phone, grant this daemon Accessibility \
                             in System Settings > Privacy & Security > Accessibility",
                        );
                    }
                }
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
        // A fresh task never inherits a previous task's sensitive-app consent.
        self.turn_allowances
            .lock()
            .expect("turn_allowances lock poisoned")
            .clear();
        self.run_or_queue(request_id, text, context_id).await;
    }

    /// The busy/queue discipline shared by every turn-starting path (`handle_prompt`,
    /// `handle_resume`, `handle_redirect`, consent-allow resume): run the turn now if no
    /// turn is in flight, else queue it. Factored out of `handle_prompt` when pause/resume/
    /// redirect grew their own turn-starting call sites.
    async fn run_or_queue(&self, request_id: String, text: String, context_id: Option<String>) {
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
        // Current-turn tracking for Stop/Pause/Redirect + the sensitive-app watchdog: `text`
        // here is still the ORIGINAL instruction (env-context augmentation happens below), so
        // a pause stash replays what the user actually asked for. Per-turn consent allowances
        // reset with the turn they were granted for.
        *self.current_turn.lock().expect("current_turn lock poisoned") = Some(CurrentTurn {
            request_id: request_id.clone(),
            text: text.clone(),
            context_id: context_id.map(str::to_owned),
            a2a_task_id: None,
        });
        // NOTE: `turn_allowances` is deliberately NOT cleared here. A resumed/consent-allowed
        // turn re-enters through this same path, and clearing at run start would wipe the
        // allowance `resolve_consent` just granted -- the watchdog would re-ask on its very
        // next tick, an ask-allow-ask loop (latent-bug shape caught during the live consent
        // witness). Allowances are cleared where a genuinely NEW task begins instead:
        // `handle_prompt` and `handle_redirect`.
        struct ConcludeOnDrop<'a> {
            tasks: &'a crate::task_fsm::TaskRegistry,
            request_id: &'a str,
            current_turn: &'a Mutex<Option<CurrentTurn>>,
        }
        impl Drop for ConcludeOnDrop<'_> {
            fn drop(&mut self) {
                self.tasks.conclude(self.request_id);
                let mut current = self
                    .current_turn
                    .lock()
                    .expect("current_turn lock poisoned");
                if current
                    .as_ref()
                    .is_some_and(|t| t.request_id == self.request_id)
                {
                    *current = None;
                }
            }
        }
        let _conclude_task = ConcludeOnDrop {
            tasks: &self.tasks,
            request_id: &request_id,
            current_turn: &self.current_turn,
        };

        // Sensitive-app watchdog (the PRD section-9 privacy layer's live interception
        // point): polls the frontmost app for this turn's lifetime and pauses/blocks when
        // it enters a configured class-5 category. Needs an `Arc` to outlive this call
        // frame, which only exists once `main.rs` has attached the bridge backref --
        // probe/bare-lib callers simply run without the watchdog (degrade-don't-crash).
        if let Some(bridge) = self.bridge.get().and_then(std::sync::Weak::upgrade) {
            tokio::spawn(sensitive_watchdog(bridge, request_id.clone()));
        }

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
        let env_block = match &self.env_context {
            Some(store) => match store.retrieve(&text, 5).await {
                Ok(facts) => crate::env_context::format_context_block(&facts),
                Err(err) => {
                    tracing::debug!(
                        request_id,
                        error = %format!("{err:#}"),
                        "env_context retrieval failed; sending prompt without environment context"
                    );
                    None
                }
            },
            None => None,
        };

        // Process-awareness / do-not-touch guard (issue 2): the HARD, UNCONDITIONAL rules
        // (never interrupt an existing Claude Code session, default terminal is Ghostty) plus
        // a LIVE snapshot of the protected processes running right now. Injected on every turn
        // ahead of the softer semantically-retrieved env facts and the user's own prompt, so
        // the agent is always told exactly what is running and what it must not disturb. Runs
        // synchronously (a quick `ps`); best-effort inside the module (empty on failure).
        let guard_block = crate::process_awareness::guard_block_now();
        // Task-execution framing (see `crate::agent_guidance`): tells the agent to
        // ACT on the request in full and that pre-existing similar content (e.g. the
        // user's own earlier "hi" messages on Slack) is NOT task completion. Injected
        // unconditionally every turn, like the guard block.
        let task_framing = crate::agent_guidance::task_framing_block();

        // Order: hard guard first (highest authority), then task-execution framing,
        // then durable env facts, then the user's instruction. `run_prompt_once`
        // sends this whole string to the backend.
        let augmented_text = {
            let mut s = guard_block;
            s.push('\n');
            s.push_str(task_framing);
            if let Some(block) = env_block {
                s.push('\n');
                s.push_str(&block);
            }
            s.push('\n');
            s.push_str(&text);
            s
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
                            // The retry runs against a RESPAWNED holo serve: attempt 0's
                            // resolved contextId/Task.id are meaningless there (live-hit:
                            // a scoped cancel against the new process with the stale task
                            // id returned "Task not found" and had to fall back to the
                            // global stop). Reset so the retry's own stream re-resolves
                            // fresh ids for any later Stop/Pause/Redirect to target.
                            {
                                let mut current = self
                                    .current_turn
                                    .lock()
                                    .expect("current_turn lock poisoned");
                                if let Some(turn) = current
                                    .as_mut()
                                    .filter(|t| t.request_id == request_id)
                                {
                                    turn.context_id = None;
                                    turn.a2a_task_id = None;
                                }
                            }
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
            .send_and_stream(
                &text,
                context_id,
                |ids| {
                    // Scoped-cancel enablement: record the resolved contextId AND the A2A
                    // Task.id the moment the stream reveals them, so Stop/Pause/Redirect
                    // arriving mid-turn can issue a targeted `tasks/cancel` (keyed by task
                    // id -- the only key this holo serve accepts) instead of only the
                    // global kill switch.
                    let mut current = self
                        .current_turn
                        .lock()
                        .expect("current_turn lock poisoned");
                    if let Some(turn) = current
                        .as_mut()
                        .filter(|t| t.request_id == request_id_for_updates)
                    {
                        if turn.context_id.is_none() {
                            if let Some(ctx) = ids.context_id {
                                turn.context_id = Some(ctx.to_string());
                            }
                        }
                        if turn.a2a_task_id.is_none() {
                            if let Some(tid) = ids.task_id {
                                turn.a2a_task_id = Some(tid.to_string());
                            }
                        }
                        tracing::debug!(
                            request_id = request_id_for_updates,
                            context_id = ?turn.context_id,
                            a2a_task_id = ?turn.a2a_task_id,
                            "turn ids resolved mid-stream"
                        );
                    }
                },
                |update| {
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
                // Lock ORDER matters and must match `run_or_queue`'s (busy, then queue):
                // the original queue-then-busy order here was a latent AB-BA deadlock that
                // only stayed unreachable while the control channel's read loop serialized
                // every `handle` call -- now that turns are spawned and control verbs run
                // concurrently, two lock takers genuinely race.
                let mut busy = self.busy.lock().expect("busy lock poisoned");
                let mut queue = self.queue.lock().expect("queue lock poisoned");
                let next = queue.pop_front();
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

        // The request_id of the turn actually running when this Stop arrived -- captured now
        // so the force-escalation below can tell "the turn we asked to stop is STILL running"
        // from "it stopped and a different turn started". `None` if nothing was running.
        let stopped_turn_request_id = self
            .current_turn
            .lock()
            .expect("current_turn lock poisoned")
            .as_ref()
            .map(|t| t.request_id.clone());

        // A stop discards any paused turn outright -- "stop" means stop everything, and a
        // stash silently surviving it would resurrect a task the user just killed on the
        // next Resume. Also drop any outstanding consent request tied to it.
        if self
            .paused
            .lock()
            .expect("paused lock poisoned")
            .take()
            .is_some()
        {
            *self
                .pending_consent
                .lock()
                .expect("pending_consent lock poisoned") = None;
            self.emit_daemon_status("stop: discarded the paused task");
        }

        // Scoped cancel of the CURRENT turn when its contextId is already known, even if the
        // wire message carried no context_id (the iOS client never has one): lower blast
        // radius and a faster terminal than waiting on the global kill switch's 250ms file
        // poll alone. The global `holo stop` below still runs for the no-context case.
        // Debug-only witness hook (see the block further down): when set, skip BOTH the
        // scoped A2A cancel and the graceful holo stop so the turn genuinely survives and the
        // force escalation is forced to fire. Never set in production.
        let skip_graceful = std::env::var("HOLOIROH_DEBUG_STOP_SKIP_GRACEFUL")
            .map(|v| v == "1")
            .unwrap_or(false);

        let current_ids = self
            .current_turn
            .lock()
            .expect("current_turn lock poisoned")
            .as_ref()
            .and_then(|t| t.context_id.clone().map(|ctx| (ctx, t.a2a_task_id.clone())));
        if context_id.is_none() && !skip_graceful {
            if let Some((ctx, task_id)) = current_ids {
                let client = self.client.read().expect("client lock poisoned").clone();
                if let Err(err) = client.cancel(&ctx, task_id.as_deref()).await {
                    tracing::warn!(request_id, context_id = %ctx, error = %err, "scoped cancel of current turn failed (global stop still follows)");
                }
            }
        }

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

        // Debug-only witness hook: `HOLOIROH_DEBUG_STOP_SKIP_GRACEFUL=1` makes this Stop skip
        // the scoped A2A cancel (above) AND the graceful `holo stop` (below), so the running
        // turn survives and the force escalation is forced to fire against a genuinely-
        // still-running turn -- the only way to witness the escalation path end-to-end without
        // a real runaway desktop agent. Never set in production.
        if skip_graceful {
            tracing::warn!("HOLOIROH_DEBUG_STOP_SKIP_GRACEFUL set -- skipping scoped cancel + graceful holo stop to exercise force escalation");
        }

        // Global `holo stop` kill switch: always issued when no context_id was given (the
        // caller wants "stop whatever is running"), or when `force` was requested (force
        // implies a process-level SIGKILL that only the CLI path performs -- see stop.rs).
        if (context_id.is_none() || force) && !skip_graceful {
            if let Err(err) = crate::holo_bridge::stop::holo_stop(&self.holo_bin, force).await {
                tracing::warn!(request_id, error = %err, "holo stop failed");
                self.emit(ControlEvent::Error {
                    request_id: request_id.clone(),
                    message: format!("holo stop failed: {err}"),
                });
                return;
            }
        }

        // FORCE ESCALATION (the fix for "I pressed Stop and the agent kept going"): graceful
        // `holo stop` is best-effort -- it files `~/.holo/stop` and relies on the running
        // turn's own 250ms poll to pause-then-cancel the backend session. For a real agent
        // mid-tool-execution that can fail to actually halt, which is exactly what the user
        // witnessed. A user-initiated Stop is an explicit "kill it NOW", so if the SAME turn
        // is still running a short moment after the graceful stop, escalate to
        // `holo stop --force` (SIGKILL of `hai-agent-runtime`); the health loop respawns
        // `holo serve` for the next prompt. Skipped when this stop was already `force`, or
        // when nothing was running.
        if !force {
            if let (Some(stopped_id), Some(bridge)) = (
                stopped_turn_request_id.clone(),
                self.bridge.get().and_then(std::sync::Weak::upgrade),
            ) {
                tokio::spawn(async move {
                    // 3s (not a snappy 1.5s): the scoped A2A cancel + graceful `holo stop`
                    // above genuinely halt the backend for normal turns (witnessed: 0 progress
                    // after the canceled terminal), so a turn STILL running 3s after an
                    // explicit user Stop is a real runaway that earns the hammer. The generous
                    // window makes a spurious force-kill of an about-to-finish turn (which
                    // would cost an unnecessary `holo serve` restart) very unlikely.
                    tokio::time::sleep(std::time::Duration::from_millis(3000)).await;
                    let still_running = bridge
                        .control
                        .current_turn
                        .lock()
                        .expect("current_turn lock poisoned")
                        .as_ref()
                        .is_some_and(|t| t.request_id == stopped_id);
                    if still_running {
                        tracing::warn!(
                            request_id = stopped_id,
                            "stop: turn still running 3s after graceful holo stop; escalating to `holo stop --force`"
                        );
                        if let Err(err) =
                            crate::holo_bridge::stop::holo_stop(&bridge.control.holo_bin, true).await
                        {
                            tracing::warn!(error = %err, "forced holo stop failed");
                        } else {
                            bridge.control.emit_daemon_status(
                                "stop escalated to force -- the agent was still running and has been killed",
                            );
                        }
                    }
                });
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

    /// Cancel the in-flight turn as gracefully as its known state allows: a scoped A2A
    /// `tasks/cancel` when the turn's `contextId` has resolved, else the graceful (non-force)
    /// global `holo stop` kill switch. Shared by Pause, Redirect, and the sensitive-app
    /// watchdog's HardBlock arm. Best-effort by design: a failed cancel is logged and
    /// surfaced, never a panic -- the turn's own stream error/terminal handling remains the
    /// backstop.
    async fn cancel_current_turn(&self, why: &str) {
        let current = self
            .current_turn
            .lock()
            .expect("current_turn lock poisoned")
            .clone();
        let Some(turn) = current else { return };
        match turn.context_id.as_deref() {
            Some(ctx) => {
                let client = self.client.read().expect("client lock poisoned").clone();
                if let Err(err) = client.cancel(ctx, turn.a2a_task_id.as_deref()).await {
                    tracing::warn!(request_id = turn.request_id, context_id = ctx, error = %err, why, "scoped cancel failed; falling back to global holo stop");
                    if let Err(err) = crate::holo_bridge::stop::holo_stop(&self.holo_bin, false).await {
                        tracing::warn!(error = %err, why, "global holo stop also failed");
                        self.emit_daemon_status(format!("{why}: could not stop the running turn: {err}"));
                    }
                }
            }
            None => {
                if let Err(err) = crate::holo_bridge::stop::holo_stop(&self.holo_bin, false).await {
                    tracing::warn!(error = %err, why, "holo stop failed");
                    self.emit_daemon_status(format!("{why}: could not stop the running turn: {err}"));
                }
            }
        }
    }

    /// `Pause`: park the in-flight turn so `Resume` can continue it. The Holo backend has no
    /// pause RPC (its own kill switch is pause-then-cancel -- see `stop.rs`'s source notes),
    /// so the only honest pause available is: cancel the running turn NOW, remember its
    /// instruction + backend session (`contextId`), and let Resume re-dispatch on that same
    /// session, whose history carries the progress made so far.
    async fn handle_pause(&self, request_id: String) {
        self.emit(ControlEvent::Ack {
            request_id: request_id.clone(),
        });

        if self.paused.lock().expect("paused lock poisoned").is_some() {
            self.emit_daemon_status("already paused -- send resume to continue, or redirect/stop to replace it");
            return;
        }
        let current = self
            .current_turn
            .lock()
            .expect("current_turn lock poisoned")
            .clone();
        let Some(turn) = current else {
            self.emit_daemon_status("nothing is running -- no task to pause");
            return;
        };

        *self.paused.lock().expect("paused lock poisoned") = Some(PausedTurn {
            request_id: turn.request_id.clone(),
            text: turn.text.clone(),
            context_id: turn.context_id.clone(),
            auto: false,
        });
        self.cancel_current_turn("pause").await;
        self.emit_daemon_status("task paused -- send resume to continue");
    }

    /// `Resume`: continue the parked turn on the SAME backend session. The resumed turn runs
    /// under `request_id` (the resume message's own id) so the client's log/UI correlates
    /// events with the action it just took.
    async fn handle_resume(&self, request_id: String) {
        self.emit(ControlEvent::Ack {
            request_id: request_id.clone(),
        });

        let Some(parked) = self.paused.lock().expect("paused lock poisoned").take() else {
            self.emit_daemon_status("nothing is paused -- no task to resume");
            return;
        };
        // A pending consent request attached to the parked turn is now moot -- resuming IS
        // the user's decision to continue (the watchdog re-asks if the sensitive app is
        // still frontmost).
        *self
            .pending_consent
            .lock()
            .expect("pending_consent lock poisoned") = None;
        self.emit_daemon_status("resuming the paused task");
        self.redispatch_parked(
            request_id,
            parked.text,
            parked.context_id,
            "Continue the previous task from where it was paused.",
        )
        .await;
    }

    /// Re-dispatch a parked turn on its original backend session (`context_id`
    /// preserved -> history intact), prefixing `preamble` to the original
    /// instruction. Shared by user Resume and cooperative auto-yield resume so
    /// the two re-dispatch paths can never drift apart.
    async fn redispatch_parked(
        &self,
        request_id: String,
        parked_text: String,
        parked_context: Option<String>,
        preamble: &str,
    ) {
        let text = format!("{preamble} Original instruction: {parked_text}");
        self.run_or_queue(request_id, text, parked_context).await;
    }

    /// `Redirect`: replace everything in flight/queued with a new instruction, keeping the
    /// backend session so the agent retains what it had already learned/done for this task.
    async fn handle_redirect(&self, request_id: String, text: String) {
        self.emit(ControlEvent::Ack {
            request_id: request_id.clone(),
        });

        let trimmed = text.trim().to_owned();
        if trimmed.is_empty() {
            self.emit(ControlEvent::Error {
                request_id,
                message: "redirect requires a non-empty prompt".to_owned(),
            });
            return;
        }

        // Queued prompts are superseded, same drain-with-terminal treatment as Stop.
        let dropped: Vec<QueuedPrompt> = {
            let mut queue = self.queue.lock().expect("queue lock poisoned");
            queue.drain(..).collect()
        };
        for dropped_prompt in dropped {
            self.emit(ControlEvent::Done {
                request_id: dropped_prompt.request_id,
                context_id: dropped_prompt.context_id,
                status: DoneStatus::Canceled,
                message: Some("canceled: superseded by a redirect".to_owned()),
            });
        }

        // A parked (paused) turn is simply replaced, and a redirect is a new
        // instruction -- prior consent does not carry over.
        let parked = self.paused.lock().expect("paused lock poisoned").take();
        *self
            .pending_consent
            .lock()
            .expect("pending_consent lock poisoned") = None;
        self.turn_allowances
            .lock()
            .expect("turn_allowances lock poisoned")
            .clear();

        // Inherit the session: prefer the RUNNING turn's contextId, else a parked turn's --
        // whichever task the user is steering, its history should carry into the new
        // instruction.
        let inherited_context = self
            .current_turn
            .lock()
            .expect("current_turn lock poisoned")
            .as_ref()
            .and_then(|t| t.context_id.clone())
            .or_else(|| parked.and_then(|p| p.context_id));

        let had_active = self
            .current_turn
            .lock()
            .expect("current_turn lock poisoned")
            .is_some();
        if had_active {
            self.cancel_current_turn("redirect").await;
            self.emit_daemon_status(
                "redirecting: canceling the current task, your new instruction runs next",
            );
        }

        // `run_or_queue` handles the still-busy window naturally: the redirect prompt queues
        // behind the dying turn and `drain_queue` starts it the moment the cancel's terminal
        // lands -- no race, no sleep.
        self.run_or_queue(request_id, trimmed, inherited_context)
            .await;
    }

    /// Called on every `stall_watchdog` tick (see that module's doc for the full design and
    /// why this daemon-owned supervisory layer is the reachable "fan out agents" mechanism
    /// given `holo serve`'s own reasoning loop is closed-source). Checks the CURRENTLY RUNNING
    /// turn (if any) against its `TaskFsm`'s staleness, and on a genuine stall, cancels it and
    /// redispatches a self-correction nudge on the SAME backend session -- reusing
    /// [`Self::handle_redirect`] exactly, the same cancel-then-continue path `Redirect`
    /// already uses, just triggered by the daemon instead of the user. A no-op turn (nothing
    /// running, or the running turn isn't stalled, or it was already nudged within the
    /// cooldown) costs one lock-and-check, no cancel, no emit.
    pub async fn maybe_nudge_stalled_turn(&self) {
        let Some(request_id) = self
            .current_turn
            .lock()
            .expect("current_turn lock poisoned")
            .as_ref()
            .map(|t| t.request_id.clone())
        else {
            return;
        };

        let now = holoiroh_wire::epoch_millis_now();
        let should_nudge = self
            .tasks
            .with_task(&request_id, |fsm| {
                let should = fsm.should_nudge(
                    now,
                    STALL_WATCHDOG_WINDOW.as_millis() as u64,
                    STALL_WATCHDOG_NUDGE_COOLDOWN.as_millis() as u64,
                );
                if should {
                    fsm.mark_nudged(now);
                }
                should
            })
            .unwrap_or(false);
        if !should_nudge {
            return;
        }

        tracing::info!(request_id, "stall watchdog: nudging a stalled turn to self-correct");
        self.emit_daemon_status(format!(
            "{STALL_WATCHDOG_STATUS_MARKER} no progress detected -- asking the agent to verify and fix its last step"
        ));
        // Force the stronger model tier for the correction attempt, deterministically -- the
        // nudge text's own (short, low-signal) wording must never accidentally leave the
        // router on/downgrading to the simple tier right when the agent most needs the
        // stronger one. Best-effort: a failed escalation still lets the nudge itself proceed.
        if let Some(bridge) = self.bridge.get().and_then(std::sync::Weak::upgrade) {
            if let Err(err) = bridge.force_tier(crate::router::Tier::Complex).await {
                tracing::warn!(
                    request_id,
                    error = %format!("{err:#}"),
                    "stall watchdog: forcing the complex model tier failed; nudging anyway"
                );
            }
        }
        self.handle_redirect(
            format!("watchdog-nudge-{}", uuid::Uuid::new_v4()),
            STALL_WATCHDOG_NUDGE_TEXT.to_string(),
        )
        .await;
    }

    /// Resolve a wire `InputResponse` against the outstanding sensitive-app consent request,
    /// if any. Returns `true` when this response WAS that consent decision (the control
    /// channel then acks it and does not fall through to its generic pending-input logic).
    ///
    /// Associated fn taking the owning [`super::HoloBridge`] `Arc` (rather than `&self`)
    /// because the allow path re-dispatches the parked turn -- a whole streaming turn that
    /// must outlive the control channel's read-loop iteration, so it is spawned onto its own
    /// task holding a real `Arc`.
    pub fn resolve_consent(
        bridge: &Arc<super::HoloBridge>,
        request_id: &str,
        selected_option: &str,
    ) -> bool {
        let ctrl = &bridge.control;
        let pending = {
            let mut guard = ctrl
                .pending_consent
                .lock()
                .expect("pending_consent lock poisoned");
            match guard.as_ref() {
                Some(p) if p.consent_request_id == request_id => guard.take(),
                _ => None,
            }
        };
        let Some(pending) = pending else {
            return false;
        };

        if selected_option == CONSENT_ALLOW_ONCE {
            ctrl.turn_allowances
                .lock()
                .expect("turn_allowances lock poisoned")
                .insert(pending.category_id.clone());
            let parked = ctrl.paused.lock().expect("paused lock poisoned").take();
            match parked {
                Some(parked) => {
                    ctrl.emit_daemon_status(format!(
                        "consent granted for {} -- resuming the task",
                        pending.category_id
                    ));
                    let bridge = bridge.clone();
                    tokio::spawn(async move {
                        let resume_text = format!(
                            "Continue the previous task from where it was paused. Original instruction: {}",
                            parked.text
                        );
                        bridge
                            .control
                            .run_or_queue(parked.request_id, resume_text, parked.context_id)
                            .await;
                    });
                }
                None => {
                    // Consent arrived but the stash is gone (a Stop/Redirect raced it) --
                    // nothing to resume; the allowance still stands for any current turn.
                    ctrl.emit_daemon_status(format!(
                        "consent granted for {}, but the paused task is gone (stopped or redirected meanwhile)",
                        pending.category_id
                    ));
                }
            }
        } else {
            // Anything other than the allow option -- including the explicit stop option --
            // is a deny: the parked turn is discarded (it was already canceled when the gate
            // paused it).
            *ctrl.paused.lock().expect("paused lock poisoned") = None;
            ctrl.emit_daemon_status(format!(
                "consent denied for {} -- task stopped",
                pending.category_id
            ));
        }
        true
    }
}

/// Per-turn sensitive-app watchdog: the live interception point wiring
/// [`crate::sensitive_categories`] (data model) + [`crate::frontmost_app`] (live signal)
/// into the PRD section-9 behavior its module docs used to disclaim as unimplemented.
/// Polls the frontmost app once a second for the turn's lifetime:
///
/// - `AlwaysAllow` category (or no category): keep watching.
/// - `HardBlock` category: announce + cancel the turn. Terminal for the watchdog.
/// - `AlwaysAsk` category (the PRD default): pause the turn (same stash `Resume` uses) and
///   emit a `sensitive_access_consent` input request; the user's `InputResponse` resolves it
///   via [`HoloControlBridge::resolve_consent`]. One "Allow once" covers that category for
///   the REST of the same turn (`turn_allowances`), so the gate doesn't re-fire every tick.
///
/// Frontmost-app granularity is an honest heuristic, not a per-screen classifier -- see
/// `frontmost_app`'s and `sensitive_categories`' module docs for exactly what it can and
/// cannot distinguish.
async fn sensitive_watchdog(bridge: Arc<super::HoloBridge>, request_id: String) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let ctrl = &bridge.control;

        // Turn over (or replaced by a newer one)? Watchdog dies with it.
        let still_ours = ctrl
            .current_turn
            .lock()
            .expect("current_turn lock poisoned")
            .as_ref()
            .is_some_and(|t| t.request_id == request_id);
        if !still_ours {
            return;
        }

        let Some(bundle_id) = crate::frontmost_app::frontmost_bundle_id().await else {
            continue;
        };
        let classified = {
            let cats = ctrl
                .sensitive_categories
                .lock()
                .expect("sensitive_categories lock poisoned");
            cats.classify(&bundle_id)
                .map(|c| (c.id.clone(), c.display_name.clone(), c.setting))
        };
        let Some((category_id, display_name, setting)) = classified else {
            continue;
        };

        match setting {
            CategorySetting::AlwaysAllow => continue,
            CategorySetting::HardBlock => {
                tracing::warn!(
                    request_id,
                    bundle_id,
                    category = category_id,
                    "sensitive-app watchdog: hard-blocked category frontmost; stopping the turn"
                );
                ctrl.emit_daemon_status(format!(
                    "privacy: {display_name} ({bundle_id}) is hard-blocked -- stopping the task"
                ));
                ctrl.cancel_current_turn("privacy hard-block").await;
                return;
            }
            CategorySetting::AlwaysAsk => {
                if ctrl
                    .turn_allowances
                    .lock()
                    .expect("turn_allowances lock poisoned")
                    .contains(&category_id)
                {
                    continue;
                }
                tracing::info!(
                    request_id,
                    bundle_id,
                    category = category_id,
                    "sensitive-app watchdog: ask-gated category frontmost; pausing for consent"
                );

                // Park the turn exactly the way `Pause` does, then ask.
                let current = ctrl
                    .current_turn
                    .lock()
                    .expect("current_turn lock poisoned")
                    .clone();
                let Some(turn) = current else { return };
                *ctrl.paused.lock().expect("paused lock poisoned") = Some(PausedTurn {
                    request_id: turn.request_id.clone(),
                    text: turn.text.clone(),
                    context_id: turn.context_id.clone(),
                    // Consent-gate safe-pause, not cooperative auto-yield: the user
                    // must make the consent decision, so this is never auto-resumed.
                    auto: false,
                });
                ctrl.cancel_current_turn("privacy consent gate").await;

                let consent_request_id = uuid::Uuid::new_v4().to_string();
                *ctrl
                    .pending_consent
                    .lock()
                    .expect("pending_consent lock poisoned") = Some(PendingConsent {
                    consent_request_id: consent_request_id.clone(),
                    category_id: category_id.clone(),
                });
                let expires_at = holoiroh_wire::epoch_millis_now()
                    .saturating_add(CONSENT_TTL.as_millis() as u64);
                ctrl.emit(ControlEvent::InputRequested {
                    request_id: consent_request_id.clone(),
                    kind: InputRequestKind::SensitiveAccessConsent,
                    context: format!(
                        "The agent is about to work in {display_name} ({bundle_id}), a sensitive app category. The task is paused -- allow it to continue there?"
                    ),
                    response_options: vec![
                        CONSENT_ALLOW_ONCE.to_string(),
                        CONSENT_STOP_TASK.to_string(),
                    ],
                    expires_at,
                });

                // Consent expiry: if unanswered, the request lapses into the standard
                // safe-pause state -- the task simply STAYS paused (Resume re-asks if the
                // app is still sensitive-frontmost). Never an error, per the wire schema's
                // own expiry-to-safe-pause contract.
                let expiry_bridge = bridge.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(CONSENT_TTL).await;
                    let ctrl = &expiry_bridge.control;
                    let expired = {
                        let mut guard = ctrl
                            .pending_consent
                            .lock()
                            .expect("pending_consent lock poisoned");
                        match guard.as_ref() {
                            Some(p) if p.consent_request_id == consent_request_id => {
                                guard.take().is_some()
                            }
                            _ => false,
                        }
                    };
                    if expired {
                        ctrl.emit_daemon_status(holoiroh_wire::input_request_expired_text(
                            &consent_request_id,
                        ));
                    }
                });
                return;
            }
        }
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
