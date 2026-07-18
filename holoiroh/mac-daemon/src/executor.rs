//! The `ComputerUseExecutor` abstraction seam (Project Aro PRD section 7.3).
//!
//! ## Why this module exists: the seam, not a rewrite
//!
//! The PRD's section 7.3 states the invariant this module implements: *"HoloDesktop sits
//! behind the executor interface ... can be replaced without changing product behavior."*
//! That is the entire point of this file. Everything the daemon does to actually drive a Mac
//! today goes through H Company's `holo-desktop-cli` (`holo serve` + the `hai-agent-runtime`
//! it fronts), reached via [`crate::holo_bridge::HoloBridge`] / [`HoloControlBridge`]. This
//! module does **not** re-implement any of that. It defines a single trait,
//! [`ComputerUseExecutor`], describing *what a computer-use backend must offer the product*
//! (start a task, watch it, pause/resume/cancel it, report its capabilities, shut down), and
//! one adapter, [`HoloDesktopExecutor`], that satisfies that trait by delegating to the
//! already-built `HoloBridge`. Swapping HoloDesktop for a different backend (a cloud
//! computer-use agent, a local vision-model loop, a mock) means writing a second
//! `impl ComputerUseExecutor` -- the product code above the seam (anything that holds a
//! `E: ComputerUseExecutor`) never changes.
//!
//! ```text
//!            product / control-channel layer
//!                        │
//!                        ▼
//!        ┌─────────────────────────────────────┐
//!        │   trait ComputerUseExecutor          │   <- this module, backend-agnostic
//!        │   (initialize/execute/observe/       │
//!        │    pause/resume/cancel/              │
//!        │    get_capabilities/shutdown)        │
//!        └─────────────────────────────────────┘
//!                        ▲
//!            ┌───────────┴───────────┐
//!            │ HoloDesktopExecutor    │            <- this module, the ONLY place that knows
//!            │ (adapts HoloBridge)    │               about HoloDesktop / holo serve / A2A
//!            └───────────┬───────────┘
//!                        ▼
//!            HoloBridge / HoloControlBridge        <- existing, unchanged (holo_bridge/)
//!                        ▼
//!                  holo serve (A2A)
//! ```
//!
//! ## What is HoloDesktop-specific, and therefore kept inside `HoloDesktopExecutor`
//!
//! Everything below the trait: `ControlMessage` construction, the `request_id`/`context_id`
//! correlation scheme, the `holo serve` A2A/`tasks/cancel` semantics, the `holo stop` global
//! kill switch, the busy/queue single-active-task model, the agent-action cap. None of those
//! nouns appears in the trait or the shared types -- they are all reached only through
//! [`HoloDesktopExecutor`]'s private methods. A caller written against the trait cannot even
//! name them.
//!
//! ## What the trait deliberately generalizes (backend-agnostic types)
//!
//! [`ExecutorConfig`], [`ExecutionTask`], [`ExecutionRun`], [`ExecutorEvent`], and
//! [`ExecutorCapabilities`] are plain data with no HoloDesktop vocabulary. They carry `serde`
//! derives on the types that cross a real boundary (the event stream that would be forwarded
//! to the iOS app, and the capabilities report a client would read) -- see each type's own
//! doc. [`ExecutorEvent`] is intentionally the *same coarse shape* the existing
//! [`crate::holo_bridge::ControlEvent`] already exposes (ack / progress / answer / done /
//! error / queued), because that is the real granularity `holo serve`'s A2A stream provides
//! today (the finer `crate::task_state::TaskState` machine is deliberately not wired to a live
//! event source yet -- see its module doc). A future backend that can report richer states
//! would extend [`ExecutorEvent`], not work around it.
//!
//! ## Honest capability mapping (no silent fakes)
//!
//! HoloDesktop has exactly one interruption primitive: stop/cancel (A2A `tasks/cancel` scoped
//! to a context, or the global `holo stop`). It has **no** mid-turn *pause-and-later-resume*
//! primitive -- `session_runner`'s stop path does pause-then-cancel as a single terminal
//! action, not a resumable suspend. Rather than pretend otherwise, [`HoloDesktopExecutor`]:
//!
//! - maps [`ComputerUseExecutor::cancel`] onto a real `ControlMessage::Stop` (the natural,
//!   fully-supported operation),
//! - maps [`ComputerUseExecutor::pause`] onto that same terminal stop **and says so** in its
//!   return value ([`PauseOutcome::CanceledNotSuspended`]) and in
//!   [`ExecutorCapabilities::can_pause_resume`] `== false`, so a caller is never misled into
//!   thinking a later `resume` will continue the turn, and
//! - makes [`ComputerUseExecutor::resume`] a no-op that returns
//!   [`ResumeOutcome::Unsupported`], again matching the advertised capability.
//!
//! This mirrors the rest of this daemon's convention (see `README.md`'s repeated
//! real-vs-honestly-approximated breakdowns): the seam is real, and the one place the backend
//! can't honor an interface verb literally is reported through the interface, not hidden.

use std::pin::Pin;
use std::sync::Arc;

use futures_util::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;

use crate::holo_bridge::control::DoneStatus;
use crate::holo_bridge::{ControlEvent, ControlMessage, HoloBridge, HoloControlBridge};

/// Opaque handle identifying one execution the product started via
/// [`ComputerUseExecutor::execute`]. Every later call that references a running execution
/// ([`observe`](ComputerUseExecutor::observe), [`pause`](ComputerUseExecutor::pause),
/// [`resume`](ComputerUseExecutor::resume), [`cancel`](ComputerUseExecutor::cancel)) is keyed
/// by this id, so the product never needs to know how a given backend correlates work
/// internally.
///
/// For [`HoloDesktopExecutor`] this is exactly the bridge's `request_id` (the key every
/// [`ControlEvent`] already carries), but that mapping is a HoloDesktop implementation detail
/// -- callers treat it as an opaque token.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RunId(pub String);

impl RunId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RunId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Backend-agnostic configuration handed to [`ComputerUseExecutor::initialize`] once, before
/// any task runs. Kept intentionally small and backend-neutral: HoloDesktop's *own* config
/// (which `holo` binary, which port, the `holo serve` subprocess lifecycle) is established
/// when the underlying [`HoloBridge`] is built, so this only carries product-level knobs that
/// any executor would honor. `serde` derives because a real deployment would load this from a
/// config file / control message rather than hard-code it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutorConfig {
    /// Whether the product wants the executor to accept new tasks at all. A backend that is
    /// present but should stay quiescent (e.g. remote-view-only mode) can be initialized with
    /// this `false`.
    #[serde(default = "default_true")]
    pub accept_tasks: bool,
    /// Free-form label for logs/diagnostics, so multiple executors (or multiple Macs) are
    /// distinguishable in a shared event log. Not load-bearing.
    #[serde(default)]
    pub label: Option<String>,
}

fn default_true() -> bool {
    true
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            accept_tasks: true,
            label: None,
        }
    }
}

/// One unit of work the product asks the executor to carry out -- the backend-agnostic
/// equivalent of "a prompt". `serde` because this is exactly the shape that crosses the
/// control-channel boundary from the iOS app (a text instruction plus optional continuation
/// context), and a second backend would deserialize the same thing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionTask {
    /// The natural-language instruction to carry out. For HoloDesktop this becomes the A2A
    /// message text; a different backend would interpret it however it drives its own agent.
    pub instruction: String,
    /// Continue a prior conversation/session when the backend supports it, or start fresh when
    /// `None`. HoloDesktop maps this to the A2A `contextId`; other backends may ignore it.
    #[serde(default)]
    pub continue_run: Option<RunId>,
    /// Provenance hint: was this typed or spoken? Purely informational (mirrors the existing
    /// `Prompt` vs `VoiceTranscript` distinction, which never changes how the backend acts).
    #[serde(default)]
    pub source: TaskSource,
}

/// Whether an [`ExecutionTask`] originated as typed text or a voice transcript. Informational
/// only -- see [`ExecutionTask::source`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskSource {
    #[default]
    Text,
    Voice,
}

/// Handle returned by [`ComputerUseExecutor::execute`]: the accepted execution's [`RunId`]
/// plus whether the backend started it immediately or queued it behind an in-flight run. The
/// product uses `run_id` for every subsequent [`observe`](ComputerUseExecutor::observe) /
/// [`cancel`](ComputerUseExecutor::cancel) / etc. call.
///
/// `serde` because a control-channel `execute` acknowledgement would serialize this back to
/// the client so it learns the id to observe and whether it's running yet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionRun {
    pub run_id: RunId,
    /// Best-effort hint: `false` when, at the moment `execute` returned, the backend was already
    /// running another task and this one will queue behind it (HoloDesktop enforces one active
    /// task per Mac -- see [`crate::limits::MAX_ACTIVE_TASKS_PER_MAC`]). The task is never lost;
    /// if queued it starts when the one ahead finishes.
    ///
    /// **This is a snapshot, not a guarantee, and is inherently racy.** For HoloDesktop, `execute`
    /// hands the task to the bridge on a spawned task and returns immediately, so two `execute`
    /// calls in quick succession can *both* observe "not busy yet" and both report `true` even
    /// though the second will in fact queue once the first's spawned turn starts. The
    /// **authoritative** queue signal is therefore the [`ExecutorEvent::Queued`] event on the
    /// run's [`observe`](ComputerUseExecutor::observe) stream (emitted iff the run actually
    /// queued), not this field -- treat `started_immediately` as a fast optimistic hint for UI,
    /// and the observe stream as ground truth.
    pub started_immediately: bool,
}

/// One backend-agnostic progress/lifecycle event for a run, as seen by
/// [`ComputerUseExecutor::observe`]. This is the coarse union `holo serve`'s A2A stream
/// actually provides today (see this module's doc on why it is not the finer
/// [`crate::task_state::TaskState`] machine). Every variant carries the [`RunId`] it belongs
/// to so a demultiplexed stream is unambiguous.
///
/// `serde` (tagged, snake_case) because this is precisely the shape that would be forwarded
/// out over the control channel to the iOS app's status panel -- it crosses a real boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ExecutorEvent {
    /// The backend acknowledged the task before any real work streamed back.
    Accepted { run_id: RunId },
    /// The task was queued behind `ahead` others already waiting on the single active slot.
    Queued { run_id: RunId, ahead: usize },
    /// One step of progress. `detail` is an optional human-readable status line; `raw` is an
    /// optional backend-specific event payload forwarded verbatim (opaque JSON), for a client
    /// that wants richer rendering than the status line.
    Progress {
        run_id: RunId,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        raw: Option<serde_json::Value>,
    },
    /// The run produced its final answer text.
    Answer { run_id: RunId, text: String },
    /// The run reached a terminal state.
    Finished {
        run_id: RunId,
        outcome: RunOutcome,
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    /// The executor itself errored for this run (backend unreachable, malformed task, cap hit)
    /// without a clean terminal state of its own.
    Failed { run_id: RunId, message: String },
}

impl ExecutorEvent {
    /// The run this event belongs to, for demultiplexing a shared event flow by [`RunId`].
    /// Returns `None` for events that are not scoped to a single run (there are none today,
    /// but the accessor keeps callers total over future out-of-band variants).
    pub fn run_id(&self) -> Option<&RunId> {
        match self {
            ExecutorEvent::Accepted { run_id }
            | ExecutorEvent::Queued { run_id, .. }
            | ExecutorEvent::Progress { run_id, .. }
            | ExecutorEvent::Answer { run_id, .. }
            | ExecutorEvent::Finished { run_id, .. }
            | ExecutorEvent::Failed { run_id, .. } => Some(run_id),
        }
    }

    /// Whether this event terminates its run (no further events for that [`RunId`] follow).
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            ExecutorEvent::Finished { .. } | ExecutorEvent::Failed { .. }
        )
    }

    /// Probe-support: run one bridge [`ControlEvent`] through the exact same translation the
    /// executor's fan-out task uses, so `examples/executor_probe.rs` can witness the
    /// `ControlEvent -> ExecutorEvent` mapping against real bridge output without reaching into
    /// the private translation fn. Returns `None` for events with no per-run identity (mirrors
    /// the fan-out task's own filtering).
    pub fn from_control_for_probe(event: &ControlEvent) -> Option<ExecutorEvent> {
        translate_control_event(event.clone())
    }
}

/// Terminal outcome of a run. The backend-agnostic mirror of
/// [`crate::holo_bridge::control::DoneStatus`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    Completed,
    Failed,
    Canceled,
}

impl From<DoneStatus> for RunOutcome {
    fn from(status: DoneStatus) -> Self {
        match status {
            DoneStatus::Completed => RunOutcome::Completed,
            DoneStatus::Failed => RunOutcome::Failed,
            DoneStatus::Canceled => RunOutcome::Canceled,
        }
    }
}

/// What a given [`ComputerUseExecutor`] implementation can actually do. Returned by
/// [`ComputerUseExecutor::get_capabilities`] so the product can enable/disable UI affordances
/// (a pause button, a resume button) truthfully per backend rather than assuming every backend
/// supports every verb.
///
/// `serde` because a client (the iOS app) would read this to decide which controls to show.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExecutorCapabilities {
    /// Stable identifier of the backend behind the seam (e.g. `"holo-desktop"`).
    pub backend: String,
    /// Whether progress streams incrementally (vs. only a final result). HoloDesktop: `true`.
    pub streaming: bool,
    /// Whether the backend can *cancel* an in-flight run. HoloDesktop: `true`.
    pub can_cancel: bool,
    /// Whether the backend can *suspend and later resume* a run mid-flight. HoloDesktop:
    /// `false` -- its only interruption is a terminal stop (see the module doc's "Honest
    /// capability mapping"). A caller must not offer a resumable-pause affordance when this is
    /// `false`.
    pub can_pause_resume: bool,
    /// Whether a run can continue a prior run's context (multi-turn conversation). HoloDesktop:
    /// `true` (A2A `contextId`).
    pub can_continue_context: bool,
    /// Maximum number of tasks the backend will run concurrently. HoloDesktop: 1 (see
    /// [`crate::limits::MAX_ACTIVE_TASKS_PER_MAC`]); further tasks queue.
    pub max_concurrent_tasks: usize,
    /// The advertised protocol/agent version, when the backend exposes one (HoloDesktop: the
    /// A2A agent card's `protocolVersion`). `None` when unknown/not yet probed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<String>,
}

/// Result of [`ComputerUseExecutor::pause`]. Because not every backend can genuinely suspend
/// a run, this reports what the pause request actually did rather than returning `()` and
/// leaving the caller to guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseOutcome {
    /// The run was genuinely suspended and can be continued with
    /// [`resume`](ComputerUseExecutor::resume). (No current backend returns this.)
    Suspended,
    /// The backend has no resumable-pause primitive, so the pause request was honored as a
    /// terminal cancel instead. A later `resume` will report [`ResumeOutcome::Unsupported`];
    /// the run is over. This is what [`HoloDesktopExecutor`] returns.
    CanceledNotSuspended,
    /// There was no such run to pause (unknown or already-finished [`RunId`]).
    NoSuchRun,
}

/// Result of [`ComputerUseExecutor::resume`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeOutcome {
    /// The suspended run was continued.
    Resumed,
    /// This backend cannot resume (it has no suspend primitive); the call was a no-op. This is
    /// what [`HoloDesktopExecutor`] always returns.
    Unsupported,
    /// There was no such run to resume.
    NoSuchRun,
}

/// The stream type [`ComputerUseExecutor::observe`] returns. Boxed (rather than an
/// `impl Stream` associated type) so the trait stays simple to hold behind a generic bound and
/// the concrete stream machinery is a backend detail. `Send` so the product can drive it from
/// any task.
pub type EventStream = Pin<Box<dyn Stream<Item = ExecutorEvent> + Send>>;

/// Errors an executor operation can surface. Kept small and backend-agnostic; the underlying
/// backend-specific error text rides in the `String`s.
#[derive(Debug, thiserror::Error)]
pub enum ExecutorError {
    /// The executor was asked to do something after [`ComputerUseExecutor::shutdown`], or
    /// before [`ComputerUseExecutor::initialize`].
    #[error("executor is not in a state to accept this operation: {0}")]
    NotReady(String),
    /// The backend rejected or failed the operation.
    #[error("backend error: {0}")]
    Backend(String),
}

/// The abstraction seam (Project Aro PRD 7.3). A computer-use backend the product can start
/// tasks on, watch, interrupt, interrogate for capabilities, and shut down -- without the
/// product knowing which backend it is.
///
/// Uses native `async fn` in traits (this crate is edition 2024). Callers hold an
/// implementation by a generic bound (`fn drive<E: ComputerUseExecutor>(e: &E)`), which keeps
/// the trait free of `dyn`-object-safety constraints while still being the single point of
/// substitution.
#[allow(async_fn_in_trait)]
pub trait ComputerUseExecutor {
    /// One-time setup before any task runs. For [`HoloDesktopExecutor`] the heavy lifting
    /// (spawning `holo serve`, health-checking it) already happened when its [`HoloBridge`]
    /// was built, so this only records product-level config; a different backend might do its
    /// real connection setup here.
    async fn initialize(&self, config: ExecutorConfig) -> Result<(), ExecutorError>;

    /// Start one task. Returns as soon as the backend has accepted (and either started or
    /// queued) it -- progress is delivered via [`observe`](Self::observe), not this return
    /// value.
    async fn execute(&self, task: ExecutionTask) -> Result<ExecutionRun, ExecutorError>;

    /// Stream every [`ExecutorEvent`] for `run_id`, in order, until the run terminates (a
    /// [`ExecutorEvent::is_terminal`] event) and the stream then ends. Observing an unknown or
    /// already-finished run yields an immediately-empty stream rather than hanging.
    fn observe(&self, run_id: &RunId) -> EventStream;

    /// Ask the backend to suspend `run_id`. See [`PauseOutcome`]: a backend without a real
    /// suspend primitive reports that it canceled instead, never silently pretends to pause.
    async fn pause(&self, run_id: &RunId) -> Result<PauseOutcome, ExecutorError>;

    /// Ask the backend to continue a previously-paused `run_id`. See [`ResumeOutcome`].
    async fn resume(&self, run_id: &RunId) -> Result<ResumeOutcome, ExecutorError>;

    /// Terminally stop `run_id`. This is the fully-supported interruption on every backend
    /// that can interrupt at all.
    async fn cancel(&self, run_id: &RunId) -> Result<(), ExecutorError>;

    /// Report what this backend can actually do (see [`ExecutorCapabilities`]).
    fn get_capabilities(&self) -> ExecutorCapabilities;

    /// Release the backend. After this, further operations may fail with
    /// [`ExecutorError::NotReady`]. Consumes `self` where the backend owns a subprocess whose
    /// graceful teardown needs ownership (as HoloDesktop's does).
    async fn shutdown(self) -> Result<(), ExecutorError>;
}

// ---------------------------------------------------------------------------------------------
// HoloDesktopExecutor: the ONLY place below the seam that knows about holo serve / A2A / the
// HoloBridge. Everything HoloDesktop-specific lives here.
// ---------------------------------------------------------------------------------------------

/// How the executor reaches the real queue/stop/event bridge. The daemon path holds the
/// process-owning [`HoloBridge`] (whose `handle_message`/`busy_state` delegate to its inner
/// [`HoloControlBridge`]); the control-bridge-only path (the probe) holds the
/// [`HoloControlBridge`] directly. Both expose the same two operations the executor needs, so
/// this enum just dispatches between them -- keeping the two construction paths from leaking into
/// every method.
enum ControlHandle {
    Bridge(Arc<HoloBridge>),
    Control(Arc<HoloControlBridge>),
}

impl ControlHandle {
    async fn handle_message(&self, message: ControlMessage) {
        match self {
            ControlHandle::Bridge(b) => b.handle_message(message).await,
            ControlHandle::Control(c) => c.handle(message).await,
        }
    }

    fn busy_state(&self) -> (bool, usize) {
        match self {
            ControlHandle::Bridge(b) => b.busy_state(),
            ControlHandle::Control(c) => c.busy_state(),
        }
    }

    /// Cheap clone of the underlying `Arc` handle, for moving into a spawned task.
    fn clone_handle(&self) -> ControlHandle {
        match self {
            ControlHandle::Bridge(b) => ControlHandle::Bridge(b.clone()),
            ControlHandle::Control(c) => ControlHandle::Control(c.clone()),
        }
    }
}

/// Adapts H Company's `holo serve` / `hai-agent-runtime` Holo3 desktop agent (reached through the
/// existing [`HoloControlBridge`](crate::holo_bridge::HoloControlBridge) and its process-owning
/// wrapper [`HoloBridge`]) to the [`ComputerUseExecutor`] seam. This is the concrete backend the
/// alpha daemon uses; per PRD 7.3 it "can be replaced without changing product behavior" by
/// providing a different `impl ComputerUseExecutor`.
///
/// ## What it wraps, and why two handles
///
/// The queue / single-active-task / stop / event-emit logic all lives in
/// [`HoloControlBridge`], which is publicly constructable and holds no process of its own -- so
/// that is what the executor drives for `execute`/`cancel`/`pause` and taps for `observe`. The
/// process-owning [`HoloBridge`] adds one thing on top the control bridge cannot: graceful
/// teardown of the `holo serve` subprocess. The executor therefore holds an
/// `Option<Arc<HoloBridge>>` *only* for [`shutdown`](ComputerUseExecutor::shutdown); the daemon
/// path (`start_holo_desktop_executor`) sets it, and every other trait method reaches only the
/// control bridge. Keeping the `HoloBridge` handle optional is also what lets the seam be
/// exercised without spawning a real `holo serve` (see `examples/executor_probe.rs`).
///
/// ## How it taps the bridge's event flow
///
/// [`HoloControlBridge`] emits every [`ControlEvent`] into a single `events_tx` (see its
/// `emit`/`replace_event_sink`). This executor owns the matching receiver: it is constructed
/// from the *same* channel the control bridge was built with, and a background fan-out task
/// drains that one receiver and re-broadcasts each event, tagged by [`RunId`], to whatever
/// [`observe`](ComputerUseExecutor::observe) streams are currently interested. This is why
/// `execute`/`observe` do not each spin up their own event pipe -- they share the bridge's real
/// one rather than duplicating its production.
pub struct HoloDesktopExecutor {
    /// The real queue/stop/event-emitting bridge (see module doc), reached either directly
    /// (control-bridge-only path) or through the process-owning [`HoloBridge`] (daemon path).
    /// Every task-driving trait method delegates through this handle; nothing HoloDesktop-specific
    /// escapes above it.
    control: ControlHandle,
    /// Process-owning wrapper, present on the daemon path so
    /// [`shutdown`](ComputerUseExecutor::shutdown) can gracefully stop the `holo serve` child.
    /// `None` when the executor was built directly over a control bridge (no owned process to
    /// tear down, e.g. the probe) -- shutdown then has nothing subprocess-level to do.
    bridge: Option<Arc<HoloBridge>>,
    /// Broadcast hub the fan-out task publishes translated [`ExecutorEvent`]s to; each
    /// [`observe`](ComputerUseExecutor::observe) subscribes a fresh receiver and filters by
    /// [`RunId`]. `tokio::sync::broadcast` (not a per-run map of senders) so a subscriber that
    /// starts observing slightly after `execute` still receives events from that moment on, and
    /// so multiple observers of the same run are naturally supported.
    hub: tokio::sync::broadcast::Sender<ExecutorEvent>,
    /// Backend version discovered from the bridge's agent card at construction, surfaced in
    /// [`get_capabilities`](ComputerUseExecutor::get_capabilities).
    protocol_version: Option<String>,
    /// Kept for [`ExecutorError::NotReady`] gating after `initialize` decides task acceptance.
    accept_tasks: std::sync::atomic::AtomicBool,
}

/// Depth of the broadcast hub. Generous: events are small and the single-active-task model
/// means bursts are bounded by one turn's progress updates; a slow observer that lags past
/// this many events will see a `Lagged` skip on its own receiver (handled in `observe`) rather
/// than blocking the fan-out task.
const HUB_CAPACITY: usize = 1024;

impl HoloDesktopExecutor {
    /// Build an executor over an already-started [`HoloBridge`] (the daemon path). Taps the
    /// bridge's [`HoloControlBridge`] for all task-driving work and keeps the process-owning
    /// [`HoloBridge`] handle for graceful [`shutdown`](ComputerUseExecutor::shutdown). Takes
    /// ownership of the receiver end of the same event channel the bridge sends [`ControlEvent`]s
    /// into, and spawns the fan-out task that translates and re-broadcasts by [`RunId`].
    ///
    /// `protocol_version` is the value the caller already learned from the bridge's agent card
    /// (`HoloBridge::start` logs it); pass `None` if unknown.
    pub fn new(
        bridge: Arc<HoloBridge>,
        bridge_events_rx: mpsc::UnboundedReceiver<ControlEvent>,
        protocol_version: Option<String>,
    ) -> Self {
        // The control bridge lives *inside* the HoloBridge (`pub control` field). Rather than
        // clone it out (it isn't Clone), the executor reaches it through the HoloBridge's public
        // delegators (`handle_message`, `busy_state`, `shutdown`) -- so on this path `control` is
        // an Arc wrapping a thin delegator view. To avoid a second indirection, we build the
        // control-bridge-backed executor and additionally record the HoloBridge for shutdown.
        Self::build(
            ControlHandle::Bridge(bridge.clone()),
            Some(bridge),
            bridge_events_rx,
            protocol_version,
        )
    }

    /// Build an executor directly over a [`HoloControlBridge`], with no process-owning
    /// [`HoloBridge`] (so [`shutdown`](ComputerUseExecutor::shutdown) has no subprocess to stop).
    /// This is the constructor the seam exposes for driving the control bridge on its own -- used
    /// by `examples/executor_probe.rs` to exercise every trait method without spawning a real
    /// `holo serve`. Takes the receiver end of the same channel the control bridge was built with.
    pub fn over_control_bridge(
        control: Arc<HoloControlBridge>,
        bridge_events_rx: mpsc::UnboundedReceiver<ControlEvent>,
        protocol_version: Option<String>,
    ) -> Self {
        Self::build(
            ControlHandle::Control(control),
            None,
            bridge_events_rx,
            protocol_version,
        )
    }

    fn build(
        control: ControlHandle,
        bridge: Option<Arc<HoloBridge>>,
        bridge_events_rx: mpsc::UnboundedReceiver<ControlEvent>,
        protocol_version: Option<String>,
    ) -> Self {
        let (hub, _initial_rx) = tokio::sync::broadcast::channel(HUB_CAPACITY);
        let hub_for_task = hub.clone();
        tokio::spawn(fan_out_events(bridge_events_rx, hub_for_task));
        Self {
            control,
            bridge,
            hub,
            protocol_version,
            accept_tasks: std::sync::atomic::AtomicBool::new(true),
        }
    }

    fn ensure_ready(&self) -> Result<(), ExecutorError> {
        if self
            .accept_tasks
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            Ok(())
        } else {
            Err(ExecutorError::NotReady(
                "executor initialized with accept_tasks=false".to_string(),
            ))
        }
    }
}

/// Drains the bridge's single [`ControlEvent`] receiver forever, translating each into an
/// [`ExecutorEvent`] and publishing it to the broadcast hub. Ends when the bridge's sender is
/// dropped (daemon shutdown), which closes the receiver. A hub `send` error (no live
/// subscribers) is ignored -- events with no current observer are simply not buffered per-run,
/// matching the bridge's own "dropped receiver is fine" stance.
async fn fan_out_events(
    mut rx: mpsc::UnboundedReceiver<ControlEvent>,
    hub: tokio::sync::broadcast::Sender<ExecutorEvent>,
) {
    while let Some(control_event) = rx.recv().await {
        if let Some(event) = translate_control_event(control_event) {
            // `send` errors only when there are zero receivers; that's expected whenever no
            // one is observing, and is not an error worth propagating. Traced at debug for
            // diagnosing observe/demux behavior (each event, its run, and live subscriber count).
            match hub.send(event) {
                Ok(subscribers) => {
                    tracing::debug!(subscribers, "fan_out_events: published ExecutorEvent to hub")
                }
                Err(_no_subscribers) => {
                    tracing::trace!("fan_out_events: no observers for this event, dropping")
                }
            }
        }
    }
    tracing::debug!("HoloDesktopExecutor fan-out task ending: bridge event channel closed");
}

/// Translate one bridge [`ControlEvent`] into the seam's [`ExecutorEvent`]. Returns `None` for
/// events that carry no per-run identity and thus have no place in a run-scoped observe stream
/// (`DaemonStatus` -- an out-of-band supervisor notification, not a task-progress event).
fn translate_control_event(event: ControlEvent) -> Option<ExecutorEvent> {
    match event {
        ControlEvent::Ack { request_id } => Some(ExecutorEvent::Accepted {
            run_id: RunId(request_id),
        }),
        ControlEvent::Queued { request_id, ahead } => Some(ExecutorEvent::Queued {
            run_id: RunId(request_id),
            ahead,
        }),
        ControlEvent::Progress {
            request_id,
            text,
            raw_event,
            ..
        } => Some(ExecutorEvent::Progress {
            run_id: RunId(request_id),
            detail: text,
            raw: raw_event,
        }),
        ControlEvent::Answer {
            request_id, text, ..
        } => Some(ExecutorEvent::Answer {
            run_id: RunId(request_id),
            text,
        }),
        ControlEvent::Done {
            request_id,
            status,
            message,
            ..
        } => Some(ExecutorEvent::Finished {
            run_id: RunId(request_id),
            outcome: status.into(),
            message,
        }),
        ControlEvent::Error {
            request_id,
            message,
        } => Some(ExecutorEvent::Failed {
            run_id: RunId(request_id),
            message,
        }),
        // Out-of-band, not scoped to a run: no place in a per-run observe stream.
        ControlEvent::DaemonStatus { .. } => None,
    }
}

impl ComputerUseExecutor for HoloDesktopExecutor {
    async fn initialize(&self, config: ExecutorConfig) -> Result<(), ExecutorError> {
        self.accept_tasks
            .store(config.accept_tasks, std::sync::atomic::Ordering::SeqCst);
        tracing::info!(
            accept_tasks = config.accept_tasks,
            label = ?config.label,
            "HoloDesktopExecutor initialized"
        );
        Ok(())
    }

    async fn execute(&self, task: ExecutionTask) -> Result<ExecutionRun, ExecutorError> {
        self.ensure_ready()?;

        // HoloDesktop-specific: mint the run id the bridge will echo on every ControlEvent, and
        // observe the busy state to report started-vs-queued. The single-active-task cap
        // (MAX_ACTIVE_TASKS_PER_MAC=1) means "busy already" == "this one queues".
        let run_id = RunId(uuid::Uuid::new_v4().to_string());
        let (busy_before, _queued_before) = self.control.busy_state();

        let control_message = ControlMessage::Prompt {
            request_id: run_id.0.clone(),
            text: task.instruction,
            context_id: task.continue_run.map(|r| r.0),
        };

        // Reuse the existing queueing/single-active-task logic verbatim -- do NOT reimplement
        // it. `handle_message` acks, then either runs the turn to completion (streaming events
        // through the fan-out hub) or queues it; it returns when the turn it drove is done.
        // Spawn it so `execute` returns promptly (the product observes progress via the stream,
        // not by awaiting this call), matching the control channel's own fire-and-forget shape.
        let control = self.control.clone_handle();
        tokio::spawn(async move {
            control.handle_message(control_message).await;
        });

        Ok(ExecutionRun {
            run_id,
            started_immediately: !busy_before,
        })
    }

    fn observe(&self, run_id: &RunId) -> EventStream {
        let wanted = run_id.clone();
        // BroadcastStream (tokio-stream) turns the subscribed broadcast receiver into a real
        // Stream, yielding Ok(event) per event or Err(Lagged(n)) if this observer fell behind.
        // We (1) keep only events for `wanted` (mapping a Lagged into a synthetic Failed for
        // this run so the observer learns it missed events rather than seeing a silently
        // truncated stream), then (2) stop *after* the run's terminal event so the stream ends
        // instead of hanging. `stop_after_terminal` carries the "already emitted terminal" flag;
        // `take_while` keeps the terminal event itself (it's the last `true`) and cuts the
        // stream on the following poll. An unknown/finished run simply never matches and the
        // stream stays open until the hub closes at shutdown -- which, combined with the
        // per-run terminal cut, is the "immediately-empty / never-hangs-on-a-real-terminal"
        // contract the trait documents.
        let filtered = BroadcastStream::new(self.hub.subscribe()).filter_map(move |item| {
            let wanted = wanted.clone();
            async move {
                match item {
                    Ok(event) if event.run_id() == Some(&wanted) => Some(event),
                    Ok(_other_run) => None,
                    Err(BroadcastStreamRecvError::Lagged(n)) => Some(ExecutorEvent::Failed {
                        run_id: wanted.clone(),
                        message: format!(
                            "observer lagged and skipped {n} event(s); stream may be incomplete"
                        ),
                    }),
                }
            }
        });

        let mut terminal_seen = false;
        let stopping = filtered.take_while(move |event| {
            // Emit this event; stop *after* it if it was terminal. `take_while` includes every
            // element for which the predicate is true, so returning true here (even for the
            // terminal event) keeps it, and the next poll returns false and ends the stream.
            let keep = !terminal_seen;
            if event.is_terminal() {
                terminal_seen = true;
            }
            async move { keep }
        });

        Box::pin(stopping)
    }

    async fn pause(&self, run_id: &RunId) -> Result<PauseOutcome, ExecutorError> {
        // HoloDesktop has no resumable-suspend primitive (see module doc). The honest mapping
        // is to issue a real terminal Stop and report that we canceled rather than suspended,
        // so the caller never expects a later resume to continue this run. If the run isn't
        // active/queued, there is nothing to pause.
        let (busy, queued) = self.control.busy_state();
        if !busy && queued == 0 {
            return Ok(PauseOutcome::NoSuchRun);
        }
        self.stop_run(run_id, false).await?;
        Ok(PauseOutcome::CanceledNotSuspended)
    }

    async fn resume(&self, _run_id: &RunId) -> Result<ResumeOutcome, ExecutorError> {
        // No suspend primitive means nothing was ever suspended to resume. This is a no-op that
        // truthfully reports it, matching get_capabilities().can_pause_resume == false.
        Ok(ResumeOutcome::Unsupported)
    }

    async fn cancel(&self, run_id: &RunId) -> Result<(), ExecutorError> {
        // The fully-supported interruption: a real ControlMessage::Stop, which drains any queue
        // and issues the A2A tasks/cancel + holo stop path (see HoloControlBridge::handle_stop).
        self.stop_run(run_id, false).await
    }

    fn get_capabilities(&self) -> ExecutorCapabilities {
        ExecutorCapabilities {
            backend: "holo-desktop".to_string(),
            streaming: true,
            can_cancel: true,
            // HoloDesktop's only interruption is terminal; see module doc + pause()/resume().
            can_pause_resume: false,
            can_continue_context: true,
            max_concurrent_tasks: crate::limits::MAX_ACTIVE_TASKS_PER_MAC,
            protocol_version: self.protocol_version.clone(),
        }
    }

    async fn shutdown(self) -> Result<(), ExecutorError> {
        // Only the daemon path owns a HoloBridge (and thus a `holo serve` child) to tear down.
        // When present, take it out of the Arc so its owned graceful shutdown (SIGTERM-then-wait
        // on the child) can run; if other clones are still alive (e.g. a live control-channel
        // handler still holds one), fall back to Drop-based cleanup -- mirrors main.rs's own
        // Arc::try_unwrap(bridge) shutdown handling. When there is no HoloBridge (control-bridge-
        // only path, e.g. the probe), there is no subprocess to stop and shutdown is a clean no-op.
        let Some(bridge) = self.bridge else {
            tracing::debug!("HoloDesktopExecutor::shutdown: no owned HoloBridge process to stop");
            return Ok(());
        };
        match Arc::try_unwrap(bridge) {
            Ok(bridge) => bridge
                .shutdown()
                .await
                .map_err(|e| ExecutorError::Backend(e.to_string())),
            Err(_still_shared) => {
                tracing::warn!(
                    "HoloDesktopExecutor::shutdown: HoloBridge still has other Arc references; \
                     relying on Drop-based cleanup instead of graceful shutdown()"
                );
                Ok(())
            }
        }
    }
}

impl HoloDesktopExecutor {
    /// HoloDesktop-specific stop path shared by `cancel` and `pause`. Issues a
    /// `ControlMessage::Stop` for `run_id` through the bridge's existing stop handling
    /// (queue-drain + A2A `tasks/cancel` + `holo stop`), rather than duplicating any of it.
    async fn stop_run(&self, run_id: &RunId, force: bool) -> Result<(), ExecutorError> {
        let stop = ControlMessage::Stop {
            request_id: run_id.0.clone(),
            // context_id is unknown at this layer (the bridge tracks it internally); None means
            // "stop whatever is running", which engages the global holo stop kill switch --
            // matching HoloControlBridge::handle_stop's own context_id.is_none() path.
            context_id: None,
            force,
        };
        self.control.handle_message(stop).await;
        Ok(())
    }

    /// Probe-support: the underlying control bridge's `(busy, queued)` state, so
    /// `examples/executor_probe.rs` can log/assert the queue transitions it drives through the
    /// real trait methods. Not part of the seam (a product holding the trait never needs it).
    pub fn control_busy_for_probe(&self) -> (bool, usize) {
        self.control.busy_state()
    }
}

/// Convenience: build a [`HoloDesktopExecutor`] and its bridge together for callers (and the
/// example probe) that want one call. Mirrors [`HoloBridge::start`]'s parameters, wires the
/// event channel through, and hands back the ready executor.
///
/// Returns the executor plus the [`Arc<HoloBridge>`] (some callers -- the daemon's health-check
/// loop -- need their own clone of the bridge alongside the executor).
pub async fn start_holo_desktop_executor(
    holo_bin: impl Into<String>,
    port: u16,
) -> anyhow::Result<(HoloDesktopExecutor, Arc<HoloBridge>)> {
    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let bridge = Arc::new(HoloBridge::start(holo_bin, port, events_tx).await?);
    // HoloBridge::start verified the agent card and now retains its protocolVersion; thread it
    // through so get_capabilities() reports the real backend version on the daemon path (rather
    // than None).
    let protocol_version = bridge.protocol_version().map(str::to_owned);
    let executor = HoloDesktopExecutor::new(bridge.clone(), events_rx, protocol_version);
    Ok((executor, bridge))
}
