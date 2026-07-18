//! Project Aro PRD task lifecycle state machine.
//!
//! [`TaskState`] is the fine-grained state a single Holo-driven task moves
//! through end to end: from being created on the Mac daemon, through
//! connecting/authenticating the remote-view session, through the
//! `holo-desktop-cli` agent's own policy/app/target/draft/verify pipeline,
//! to a final user confirmation and commit. This is a *design-level*
//! lifecycle model (the PRD's own state diagram, reproduced here as a real
//! Rust type), not yet driven by any live event source in this alpha build
//! -- see "Relationship to `holo_bridge::control`" below for exactly why.
//!
//! ## Relationship to `holo_bridge::control`
//!
//! This module is deliberately independent of
//! [`crate::holo_bridge::control::ControlEvent`] and
//! [`crate::holo_bridge::control::DoneStatus`], which are the *only*
//! task-progress types actually wired to a live event source today (the
//! `holo serve` A2A stream, translated to wire [`crate::control_channel::ServerMessage`]s
//! -- see that module's `from_control_event`). Those types report three
//! coarse outcomes (`Completed` / `Failed` / `Canceled` via `DoneStatus`)
//! plus free-text `Progress`/`Answer` strings; they have no concept of
//! *which* of this module's sixteen-plus finer states a task is currently
//! in. Promoting `ControlEvent`/`DoneStatus` to carry a real [`TaskState`]
//! would require `holo-desktop-cli`'s own A2A trajectory events to expose
//! that granularity in the first place, which they do not today (see
//! `holo_bridge::a2a_client`'s module doc on what `TrajectoryEvent` opaque
//! JSON actually contains). This module is therefore built and exported
//! ready for that future wiring -- see `holoiroh/mac-daemon/src/control_channel.rs`'s
//! `ServerMessage::from_control_event` doc comment for the exact spot a
//! future change would touch once a fine-grained event source exists.
//!
//! ## Confidential Cloud (Tinfoil) states are unreachable in this alpha build
//!
//! [`TaskState::ConfidentialCloudConsentRequired`],
//! [`TaskState::ConfidentialAttestationFailed`], and
//! [`TaskState::ConfidentialModelUnavailable`] are present in this enum for
//! PRD schema completeness -- the Project Aro PRD's full state diagram
//! includes them -- but are **unreachable in this alpha build**. Tinfoil /
//! Confidential Cloud integration is deferred to the Phase 2 / beta
//! milestone per the PRD's own P0-11 requirement (alpha proceeds
//! local-only; `TINFOIL_API_KEY` is held in `mac-daemon/.env` but wired
//! into no code path this build). [`is_valid_transition`] enforces this at
//! the type level: every transition into or out of these three variants
//! returns `false` in this build, so accidentally routing a real task
//! through them is a compile-time-shaped (match-exhaustiveness-checked)
//! and run-time-checked impossibility, not just an unwritten code path.

use serde::Serialize;

/// One state in a single task's Project Aro lifecycle.
///
/// Three families of variant:
///
/// - **Flow states** (16): the linear happy path a task moves through from
///   creation to completion, in the exact order the PRD specifies:
///   [`Created`](TaskState::Created) → [`Queued`](TaskState::Queued) →
///   [`Connecting`](TaskState::Connecting) →
///   [`Authenticated`](TaskState::Authenticated) →
///   [`RemoteViewStarting`](TaskState::RemoteViewStarting) →
///   [`RemoteViewActive`](TaskState::RemoteViewActive) →
///   [`PolicyChecking`](TaskState::PolicyChecking) →
///   [`LaunchingApp`](TaskState::LaunchingApp) →
///   [`FindingTarget`](TaskState::FindingTarget) →
///   [`Navigating`](TaskState::Navigating) →
///   [`TypingDraft`](TaskState::TypingDraft) →
///   [`Verifying`](TaskState::Verifying) →
///   [`DraftReady`](TaskState::DraftReady) →
///   [`AwaitingConfirmation`](TaskState::AwaitingConfirmation) →
///   [`Committing`](TaskState::Committing) →
///   [`Completed`](TaskState::Completed).
/// - **Interactive waiting states** (4): non-terminal states that interrupt
///   a specific flow state pending a human response, then resume back into
///   that same flow state -- see [`is_valid_transition`] for the exact
///   interrupt/resume edges each one supports.
/// - **Terminal states** (10): [`is_valid_transition`] returns `false` for
///   every transition *out* of any of these -- 7 real alpha-build
///   terminals (cancellation/denial/ambiguity/not-found/rejection/timeout/
///   generic failure) plus the 3 Confidential-Cloud variants noted above,
///   which are terminal-shaped (no outgoing edges) precisely because they
///   are unreachable inbound as well in this build.
///
/// Serializes via `serde` as its PRD-specified snake_case wire name (e.g.
/// `TaskState::RemoteViewStarting` -> `"remote_view_starting"`).
//
// Not yet constructed from `main.rs`'s binary target (no live event source
// in this daemon currently tracks per-task lifecycle state at this
// granularity -- see this module's doc on the `ControlEvent` granularity
// gap) -- same "ready, not yet a call site" status as
// `control_channel::{read_line, send_on_new_stream, SharedControlChannel}`
// and `allowlist::for_probing` elsewhere in this crate. `examples/task_state_probe.rs`
// exercises it via the `[lib]` target regardless of whether the bin target
// has a call site yet.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    // --- Flow states: the linear happy path, in PRD order. ---
    /// Task record created, not yet scheduled to run.
    Created,
    /// Waiting for a prior in-flight task to finish (see
    /// `crate::holo_bridge::control::ControlEvent::Queued`, the one
    /// existing live analogue of this state -- see this module's doc on
    /// the granularity gap between the two).
    Queued,
    /// Establishing the `iroh` remote-view/control connection to the Mac.
    Connecting,
    /// Connection's auth gate passed (allowlisted device, or PIN verified
    /// -- see `crate::control_channel::ControlChannel::authenticate`).
    Authenticated,
    /// Remote-view (screen broadcast) session is being brought up.
    RemoteViewStarting,
    /// Remote-view session is live; the user can see the Mac's screen.
    RemoteViewActive,
    /// Evaluating whether the requested action is within policy before
    /// acting (may interrupt into [`NeedsConfirmation`](TaskState::NeedsConfirmation)
    /// or [`SensitiveAccessRequested`](TaskState::SensitiveAccessRequested)).
    PolicyChecking,
    /// Launching or focusing the target application.
    LaunchingApp,
    /// Locating the specific UI target (field, button, element) to act on.
    FindingTarget,
    /// Moving focus/cursor to the located target.
    Navigating,
    /// Entering the drafted text/action content.
    TypingDraft,
    /// Checking the entered draft against the original instruction/intent.
    Verifying,
    /// Draft has passed verification and is ready to show the user.
    DraftReady,
    /// Waiting for the user's explicit go-ahead before committing.
    AwaitingConfirmation,
    /// Executing the final committing action (e.g. clicking Send).
    Committing,
    /// Task finished successfully.
    Completed,

    // --- Interactive non-terminal waiting states. ---
    /// Interrupts [`Connecting`](TaskState::Connecting): the target service
    /// needs the user to log in before the connection can proceed.
    NeedsLogin,
    /// Interrupts [`Connecting`](TaskState::Connecting): the target service
    /// needs a multi-factor code before the connection can proceed.
    NeedsMfa,
    /// Interrupts [`PolicyChecking`](TaskState::PolicyChecking): the agent
    /// needs an explicit user confirmation before continuing (distinct from
    /// [`AwaitingConfirmation`](TaskState::AwaitingConfirmation), which is
    /// the one-time final-draft confirmation gate; this is a policy-time
    /// ad hoc confirmation that can occur before a draft exists at all).
    NeedsConfirmation,
    /// Interrupts [`PolicyChecking`](TaskState::PolicyChecking): the action
    /// requires access to something sensitive (e.g. a protected app or
    /// data category) and is paused pending explicit user grant.
    SensitiveAccessRequested,

    // --- Terminal alternatives (real in this alpha build). ---
    /// The user cancelled the task.
    UserCancelled,
    /// The task was denied by policy or OS-level permission.
    PermissionDenied,
    /// [`FindingTarget`](TaskState::FindingTarget) found more than one
    /// plausible target and could not disambiguate.
    AmbiguousTarget,
    /// [`FindingTarget`](TaskState::FindingTarget) found no plausible
    /// target at all.
    TargetNotFound,
    /// The user explicitly rejected a
    /// [`SensitiveAccessRequested`](TaskState::SensitiveAccessRequested)
    /// grant.
    SensitiveAccessRejected,
    /// The agent did not reach a terminal state within its allotted time.
    AgentTimeout,
    /// Generic failure not covered by a more specific terminal state.
    Failed,

    // --- Terminal alternatives: Tinfoil-deferred, unreachable in alpha. ---
    /// PRD-defined state for when Confidential Cloud execution requires
    /// fresh user consent. **Unreachable in this alpha build** -- see this
    /// module's doc comment ("Confidential Cloud (Tinfoil) states").
    ConfidentialCloudConsentRequired,
    /// PRD-defined state for when Confidential Cloud remote-attestation
    /// verification fails. **Unreachable in this alpha build.**
    ConfidentialAttestationFailed,
    /// PRD-defined state for when no Confidential Cloud model is available
    /// to serve the request. **Unreachable in this alpha build.**
    ConfidentialModelUnavailable,
}

/// Returns whether transitioning a task from `from` directly to `to` is a
/// legal move per the Project Aro PRD's task lifecycle diagram.
///
/// This is the type's actual bug-prevention surface: an invalid transition
/// attempt (skipping states on the happy path, resuming an interactive wait
/// into the wrong flow state, or emitting any edge into/out of a terminal
/// state) is a real, silent bug class this function exists to make loud
/// instead. Callers that drive a task's state forward should check this
/// before applying a transition, not merely log or ignore an invalid one.
///
/// The outer `match from` is intentionally exhaustive with no wildcard arm:
/// adding a new [`TaskState`] variant in the future forces a compile error
/// here until that variant's legal transitions are explicitly decided,
/// rather than silently defaulting to "no transitions allowed" or "all
/// transitions allowed". Wildcard (`_`) arms are used only on the *inner*
/// match (deciding which `to` values are valid from a given `from`), never
/// on this outer one.
// Not yet called from `main.rs`'s binary target -- same ready-not-wired
// status as `TaskState` itself, see that type's doc comment just above.
#[allow(dead_code)]
pub fn is_valid_transition(from: TaskState, to: TaskState) -> bool {
    use TaskState::*;

    match from {
        // --- Flow states: linear happy-path edge, plus each state's own
        // interactive-wait/terminal fan-out. ---
        Created => matches!(to, Queued | UserCancelled | Failed),

        Queued => matches!(to, Connecting | UserCancelled | Failed | AgentTimeout),

        Connecting => matches!(
            to,
            Authenticated
                | NeedsLogin
                | NeedsMfa
                | UserCancelled
                | PermissionDenied
                | AgentTimeout
                | Failed
        ),

        Authenticated => matches!(
            to,
            RemoteViewStarting | UserCancelled | PermissionDenied | AgentTimeout | Failed
        ),

        RemoteViewStarting => matches!(
            to,
            RemoteViewActive | UserCancelled | AgentTimeout | Failed
        ),

        RemoteViewActive => matches!(
            to,
            PolicyChecking | UserCancelled | AgentTimeout | Failed
        ),

        PolicyChecking => matches!(
            to,
            LaunchingApp
                | NeedsConfirmation
                | SensitiveAccessRequested
                | UserCancelled
                | PermissionDenied
                | AgentTimeout
                | Failed
        ),

        LaunchingApp => matches!(
            to,
            FindingTarget | UserCancelled | PermissionDenied | AgentTimeout | Failed
        ),

        FindingTarget => matches!(
            to,
            Navigating
                | AmbiguousTarget
                | TargetNotFound
                | UserCancelled
                | AgentTimeout
                | Failed
        ),

        Navigating => matches!(to, TypingDraft | UserCancelled | AgentTimeout | Failed),

        TypingDraft => matches!(to, Verifying | UserCancelled | AgentTimeout | Failed),

        // Verification failing sends the task back to re-navigate/re-type
        // rather than dead-ending it -- Verifying itself has no self-loop
        // (a retry re-enters via Navigating, it does not silently re-run
        // Verifying -> Verifying in place).
        Verifying => matches!(
            to,
            DraftReady | Navigating | UserCancelled | AgentTimeout | Failed
        ),

        DraftReady => matches!(to, AwaitingConfirmation | UserCancelled | AgentTimeout | Failed),

        AwaitingConfirmation => matches!(
            to,
            Committing | UserCancelled | AgentTimeout | Failed
        ),

        Committing => matches!(to, Completed | UserCancelled | AgentTimeout | Failed),

        // Completed is terminal: no outgoing edges at all.
        Completed => false,

        // --- Interactive non-terminal waiting states: each resumes only
        // into the specific flow state it interrupted (never a bypass to
        // some other state), plus its own cancel/deny/timeout fan-out. ---
        NeedsLogin => matches!(
            to,
            Connecting | UserCancelled | PermissionDenied | AgentTimeout | Failed
        ),

        NeedsMfa => matches!(
            to,
            Connecting | UserCancelled | PermissionDenied | AgentTimeout | Failed
        ),

        NeedsConfirmation => matches!(
            to,
            PolicyChecking | UserCancelled | AgentTimeout | Failed
        ),

        SensitiveAccessRequested => matches!(
            to,
            PolicyChecking
                | SensitiveAccessRejected
                | UserCancelled
                | AgentTimeout
                | Failed
        ),

        // --- Terminal alternatives (real in this alpha build): zero
        // outgoing edges, enforced explicitly rather than falling through
        // to a wildcard so each is visibly, individually a dead end. ---
        UserCancelled => false,
        PermissionDenied => false,
        AmbiguousTarget => false,
        TargetNotFound => false,
        SensitiveAccessRejected => false,
        AgentTimeout => false,
        Failed => false,

        // --- Tinfoil-deferred terminal alternatives: unreachable in this
        // alpha build, so zero outgoing edges regardless -- see module doc. ---
        ConfidentialCloudConsentRequired => false,
        ConfidentialAttestationFailed => false,
        ConfidentialModelUnavailable => false,
    }
}
