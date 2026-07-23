//! A native, per-task finite-state-machine layer over `holo serve`'s A2A event stream,
//! modeled on the PLAN/EXECUTE/VERIFY/DONE phase discipline of `AnEntrypoint/gm`'s
//! `rs-plugkit` orchestrator (`crates/plugkit-core/src/orchestrator/transitions.rs`).
//!
//! ## Why port a pattern instead of a dependency
//!
//! `rs-plugkit`'s `Phase` enum and CAS-guarded YAML PRD rows are genuinely portable IDEAS, but
//! not a linkable crate: the phase state (`transitions.rs`), PRD rows (`prd.rs`), and CAS retry
//! loop (`cas.rs`) all sit on `pkfs`, a filesystem shim whose non-wasm32 stubs are dead no-ops
//! (`pkfs.rs`) -- the crate only does real I/O inside a `wasm32-wasip1` build loaded by a
//! separate, unpublished native host (`agentplug-runner`) that implements ~26 `host_*` ABI
//! functions. There is no `cargo add rs-plugkit` path into this daemon. What DOES port cleanly,
//! because it's plain logic with no wasm/host coupling, is reimplemented here natively:
//!
//! - The linear phase chain (`Phase::next`), mirroring `transitions.rs`'s `next_phase`.
//! - A hard code-level gate before advancing to a terminal phase (mirroring `gates.rs`'s
//!   CONSOLIDATE/COMPLETE checks: real evidence required, not just "the model said so").
//! - Per-task state persisted to disk so a daemon crash/restart doesn't lose in-flight phase
//!   tracking (mirroring `.gm/turn-state.json`), at `~/.holoiroh/tasks/<request_id>.json`.
//!
//! ## Why THESE phases, grounded in real backend signal
//!
//! `holo serve`'s A2A stream already contains distinguishable `TrajectoryEvent` kinds --
//! `policy_event` (the model reasoning/deciding what to do next), `tool_result` (an action
//! actually taken on the desktop), `answer_event` (the final output) -- witnessed directly in
//! `~/.holo/runs/*/events.jsonl` during this daemon's own development. `raw_event` on
//! `TaskUpdate::Working` already carries this JSON; it was previously forwarded to the phone
//! unread (`control::translate_update`). [`Phase::from_trajectory_kind`] reads the REAL `kind`
//! field rather than inventing synthetic stage labels, so a task's phase reflects what the
//! backend is actually doing, not a guess.
//!
//! Four phases (not gm's six): `Plan` (the model is observing/reasoning, no action taken yet),
//! `Execute` (at least one real tool call has landed), `Verify` (an answer artifact arrived --
//! the backend's own claim of done, not yet confirmed), `Done`/`Failed` (a real A2A terminal
//! state closed the turn). gm's EMIT/CONSOLIDATE have no analog here -- this daemon does not
//! write files or push git commits per task, so those phases would be decorative.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// A task's phase, in the ONLY order it can ever advance through -- see [`Phase::next`].
/// Mirrors `rs-plugkit`'s `Phase` enum shape (a plain linear chain with a `next()` step
/// function), scoped to the four phases this daemon has real signal for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// The model is observing the desktop / reasoning about what to do -- no tool call has
    /// landed yet for this task. Entered on task creation and on every `policy_event`/
    /// `observation_event` seen before the first `tool_result`.
    Plan,
    /// At least one real desktop action (`tool_result`) has been taken. Sticky: further
    /// `policy_event`s during a multi-step task do NOT regress the phase back to `Plan` --
    /// planning between actions is normal mid-execution behavior, not a phase change.
    Execute,
    /// An `answer_event`/`TaskUpdate::Answer` arrived -- the backend's OWN claim that the task
    /// is done. Deliberately NOT the same as `Done`: gm's witness discipline is "a finding is
    /// only real once witnessed," and an unconfirmed self-reported answer is exactly the
    /// un-witnessed claim that discipline exists to catch. See [`TaskFsm::advance_terminal`]
    /// for the real gate: `Verify` can only close to `Done` when the transport's OWN terminal
    /// signal (`TerminalState::Completed`) independently confirms it, not the answer text alone.
    Verify,
    /// A real A2A terminal state (`TerminalState::Completed`) closed the turn. Terminal.
    Done,
    /// A real A2A terminal state (`TerminalState::Failed` or `TerminalState::Canceled`) closed
    /// the turn, or the turn was refused before any progress (see [`TaskFsm::fail`]). Terminal.
    Failed,
}

impl Phase {
    /// Whether this phase is terminal (no further transitions accepted).
    pub fn is_terminal(self) -> bool {
        matches!(self, Phase::Done | Phase::Failed)
    }

    /// Classify a real `TrajectoryEvent.kind` string (as emitted by `hai-agent-runtime`,
    /// witnessed values: `"policy_event"`, `"observation_event"`, `"tool_result"`,
    /// `"message_event"`, `"answer_event"`) into the phase it implies, when unambiguous.
    /// `None` for kinds this daemon doesn't have a phase mapping for (e.g. `message_event`,
    /// the echoed input) -- the caller leaves the phase unchanged rather than guessing.
    fn from_trajectory_kind(kind: &str) -> Option<Phase> {
        match kind {
            "policy_event" | "observation_event" => Some(Phase::Plan),
            "tool_result" => Some(Phase::Execute),
            "answer_event" => Some(Phase::Verify),
            _ => None,
        }
    }
}

/// Persisted state for one in-flight (or recently concluded) task. Serialized to
/// `~/.holoiroh/tasks/<request_id>.json` -- see [`TaskFsm::save`]/[`TaskFsm::load`]. Field
/// shape mirrors `rs-plugkit`'s `TurnState` (`transitions.rs`): phase, an identifying key
/// (`request_id` here instead of `session_id`), and a timestamp, minus the pending-skill/
/// pending-step fields that only make sense for a coding-agent's own skill-dispatch loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskFsm {
    pub request_id: String,
    pub phase: Phase,
    /// Count of real `tool_result` events seen -- the concrete "has the agent actually done
    /// anything yet" signal, surfaced to the phone as part of phase-change status text so a
    /// stuck-in-Plan task (the model keeps observing/reasoning with zero actions) is visible
    /// as a distinct, diagnosable state rather than indistinguishable generic "Working".
    pub actions_taken: u32,
    /// The answer text last seen via `TaskUpdate::Answer`, if any -- retained so a `Verify`
    /// phase can be reported with *what* the backend claimed before it's confirmed.
    pub claimed_answer: Option<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    /// When the stall watchdog (`crate::holo_bridge::stall_watchdog`) last nudged this task,
    /// if ever -- see [`Self::should_nudge`]/[`Self::mark_nudged`]. `None` until the first
    /// nudge; never touched by ordinary phase transitions (a nudge is not itself progress).
    #[serde(default)]
    pub last_nudge_ms: Option<u64>,
}

impl TaskFsm {
    /// Start tracking a new task at `Phase::Plan` -- every task begins there; only a real
    /// `tool_result` can advance it, so a task that finishes via `answer_event` with zero
    /// actions in between (a pure Q&A turn, no desktop interaction needed) correctly SKIPS
    /// `Execute` and goes straight `Plan -> Verify -> Done`. This is real skip-ahead, not a
    /// bug: unlike `rs-plugkit`'s strict linear chain (which never skips because a coding
    /// agent's PLAN/EXECUTE/EMIT/VERIFY/CONSOLIDATE steps are ALWAYS all real work), a Holo3
    /// turn that answers from context alone genuinely never executes anything.
    pub fn new(request_id: impl Into<String>) -> Self {
        let now = now_ms();
        Self {
            request_id: request_id.into(),
            phase: Phase::Plan,
            actions_taken: 0,
            claimed_answer: None,
            created_at_ms: now,
            updated_at_ms: now,
            last_nudge_ms: None,
        }
    }

    /// Feed one `TaskUpdate::Working`'s `raw_event` JSON through the phase classifier.
    /// Advance-only (`Plan -> Execute` is one-way; a later `policy_event` mid-`Execute` does
    /// NOT regress the phase -- see [`Phase::Execute`]'s doc). Returns `true` if the phase
    /// actually changed, so the caller can decide whether to emit a phase-change status line.
    pub fn observe_working(&mut self, raw_event: Option<&serde_json::Value>) -> bool {
        if self.phase.is_terminal() {
            return false;
        }
        let Some(kind) = raw_event.and_then(|e| e.get("kind")).and_then(|k| k.as_str()) else {
            return false;
        };
        if kind == "tool_result" {
            self.actions_taken += 1;
        }
        let Some(implied) = Phase::from_trajectory_kind(kind) else {
            return false;
        };
        let changed = self.advance_to(implied);
        self.updated_at_ms = now_ms();
        changed
    }

    /// Record the backend's self-reported answer -- advances to `Verify` (the un-witnessed
    /// claim state; see [`Phase::Verify`]'s doc for why this is deliberately not `Done` yet).
    pub fn observe_answer(&mut self, text: &str) -> bool {
        if self.phase.is_terminal() {
            return false;
        }
        self.claimed_answer = Some(text.to_string());
        let changed = self.advance_to(Phase::Verify);
        self.updated_at_ms = now_ms();
        changed
    }

    /// The real witness gate: close to a TERMINAL phase only on the transport's OWN signal
    /// (`TerminalState`), never on the answer text alone. Mirrors `rs-plugkit`'s `gates.rs`
    /// discipline -- "a finding is only real once witnessed by execution", here meaning the
    /// A2A layer's own completion signal is the witness, not the model's self-report parsed
    /// out of `answer_event` text. A `Completed` state reached with ZERO actions taken and NO
    /// claimed answer (the empty-completion shape `control.rs`'s failover logic already
    /// treats as a backend failure) is downgraded to `Failed` here too -- the same real signal,
    /// checked at the FSM layer instead of only in the retry/failover path.
    pub fn advance_terminal(&mut self, state: crate::holo_bridge::a2a_client::TerminalState) {
        use crate::holo_bridge::a2a_client::TerminalState as T;
        if self.phase.is_terminal() {
            return;
        }
        let target = match state {
            T::Completed if self.actions_taken == 0 && self.claimed_answer.is_none() => {
                Phase::Failed
            }
            T::Completed => Phase::Done,
            T::Failed | T::Canceled => Phase::Failed,
        };
        self.phase = target;
        self.updated_at_ms = now_ms();
    }

    /// Fail the task directly (bridge/transport error before any A2A terminal arrived --
    /// e.g. `send_and_stream` returning `Err`). No witness check needed: an explicit
    /// transport error IS the evidence.
    pub fn fail(&mut self) {
        if !self.phase.is_terminal() {
            self.phase = Phase::Failed;
            self.updated_at_ms = now_ms();
        }
    }

    /// One-way advance: only moves forward along `Plan -> Execute -> Verify -> {Done,Failed}`,
    /// never backward, and a same-or-earlier target is a no-op. This is what makes
    /// `observe_working`'s "sticky Execute" and "Plan can jump straight to Verify" behaviors
    /// both correct with one shared rule instead of two special cases.
    fn advance_to(&mut self, target: Phase) -> bool {
        if rank(target) > rank(self.phase) {
            self.phase = target;
            true
        } else {
            false
        }
    }

    /// A short human-readable phase-change line for `ControlEvent::Progress`/`DaemonStatus`,
    /// so the phone sees "planning -> acting on your Mac -> reviewing the result" instead of
    /// opaque unlabeled progress dots.
    pub fn phase_status_text(&self) -> String {
        match self.phase {
            Phase::Plan => "planning".to_string(),
            Phase::Execute => format!(
                "acting on your Mac ({} action{} so far)",
                self.actions_taken,
                if self.actions_taken == 1 { "" } else { "s" }
            ),
            Phase::Verify => "reviewing the result".to_string(),
            Phase::Done => "done".to_string(),
            Phase::Failed => "failed".to_string(),
        }
    }

    /// Whether `crate::holo_bridge::stall_watchdog` should nudge this task at `now_ms`: not
    /// terminal, no real phase advancement for at least `stall_window_ms`, and (if it was
    /// nudged before) at least `nudge_cooldown_ms` since that last nudge -- a nudge is not
    /// itself progress, so a task that's STILL stuck after one nudge is still eligible once
    /// the cooldown passes, but never nudged twice in a tight loop while the agent is
    /// genuinely working through the first nudge's correction.
    pub fn should_nudge(&self, now_ms: u64, stall_window_ms: u64, nudge_cooldown_ms: u64) -> bool {
        if self.phase.is_terminal() {
            return false;
        }
        if now_ms.saturating_sub(self.updated_at_ms) < stall_window_ms {
            return false;
        }
        match self.last_nudge_ms {
            Some(last) => now_ms.saturating_sub(last) >= nudge_cooldown_ms,
            None => true,
        }
    }

    /// Records that a nudge was just sent. Deliberately does NOT touch `updated_at_ms`/`phase`
    /// -- the nudge itself is not evidence of real progress, only a real `tool_result`/
    /// `answer_event`/terminal signal (via `observe_working`/`observe_answer`/
    /// `advance_terminal`) advances those.
    pub fn mark_nudged(&mut self, now_ms: u64) {
        self.last_nudge_ms = Some(now_ms);
    }

    // MARK: - Persistence (crash/restart survival, mirroring `.gm/turn-state.json`)

    fn tasks_dir() -> Result<PathBuf> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .context("HOME not set; cannot locate ~/.holoiroh/tasks")?;
        let dir = home.join(".holoiroh").join("tasks");
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok(dir)
    }

    fn path_for(request_id: &str) -> Result<PathBuf> {
        // request_id is a daemon-generated UUID (see control_channel's envelope handling) or a
        // probe-supplied plain string -- sanitize defensively so it can never escape the tasks
        // dir via path traversal even if a future caller passes untrusted input.
        let safe: String = request_id
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .collect();
        Ok(Self::tasks_dir()?.join(format!("{safe}.json")))
    }

    /// Persist current state. Best-effort by design at call sites (a failed save should never
    /// abort a real task turn) -- callers log the error and continue.
    pub fn save(&self) -> Result<()> {
        let path = Self::path_for(&self.request_id)?;
        let json = serde_json::to_string_pretty(self).context("serializing TaskFsm")?;
        std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))
    }

    /// Load a previously persisted task's state, if present.
    #[allow(dead_code)] // no bin-target caller yet; real API for a future "resume after crash" surface
    pub fn load(request_id: &str) -> Result<Option<Self>> {
        let path = Self::path_for(request_id)?;
        let contents = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
        };
        let fsm = serde_json::from_str(&contents).with_context(|| format!("parsing {}", path.display()))?;
        Ok(Some(fsm))
    }

    /// Delete this task's persisted state (called once a terminal phase's outcome has been
    /// fully emitted to the phone -- there's nothing to resume for a concluded task, and
    /// `~/.holoiroh/tasks/` would otherwise grow unboundedly across a long-running daemon).
    pub fn delete_persisted(&self) {
        if let Ok(path) = Self::path_for(&self.request_id) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Register storage: the daemon's active tasks, keyed by `request_id`. A daemon-wide singleton
/// (parallel to how `HoloControlBridge` already tracks `busy`/`queue` -- see `control.rs`) so
/// the FSM survives across the `run_prompt`/`run_prompt_once` boundary without threading a
/// mutable reference through every call site.
#[derive(Default)]
pub struct TaskRegistry {
    active: Mutex<std::collections::HashMap<String, TaskFsm>>,
}

impl TaskRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start tracking a new task, replacing any stale prior entry for the same `request_id`
    /// (a retried turn, e.g. the tinfoil-failover retry in `control.rs`, reuses the SAME
    /// `request_id` for its second attempt -- fresh FSM state is correct there, since the
    /// retry is a genuinely new attempt at the task, not a continuation).
    pub fn begin(&self, request_id: &str) {
        let fsm = TaskFsm::new(request_id);
        if let Err(err) = fsm.save() {
            tracing::warn!(request_id, error = %format!("{err:#}"), "failed to persist new task FSM state");
        }
        self.active
            .lock()
            .expect("task registry lock poisoned")
            .insert(request_id.to_string(), fsm);
    }

    /// Run `f` against the task's live FSM (if tracked), persisting afterward. Returns
    /// `f`'s result, or `None` if this `request_id` isn't tracked (e.g. `begin` was never
    /// called -- callers treat that as "no phase tracking for this turn", not an error).
    pub fn with_task<R>(&self, request_id: &str, f: impl FnOnce(&mut TaskFsm) -> R) -> Option<R> {
        let mut guard = self.active.lock().expect("task registry lock poisoned");
        let fsm = guard.get_mut(request_id)?;
        let result = f(fsm);
        if let Err(err) = fsm.save() {
            tracing::warn!(request_id, error = %format!("{err:#}"), "failed to persist task FSM state");
        }
        Some(result)
    }

    /// Remove a concluded task from the in-memory registry and delete its persisted file.
    /// Call once its terminal phase has been fully emitted to the phone.
    pub fn conclude(&self, request_id: &str) {
        let removed = self
            .active
            .lock()
            .expect("task registry lock poisoned")
            .remove(request_id);
        if let Some(fsm) = removed {
            fsm.delete_persisted();
        }
    }
}

fn rank(phase: Phase) -> u8 {
    match phase {
        Phase::Plan => 0,
        Phase::Execute => 1,
        Phase::Verify => 2,
        Phase::Done => 3,
        Phase::Failed => 3,
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
