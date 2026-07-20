//! Session/rate limit constants and enforcement helpers, per Project Aro PRD
//! section 10.4 ("Session & rate limits").
//!
//! This module is the single source of truth for every numeric limit named
//! in PRD 10.4. Downstream code should import these constants rather than
//! re-hardcoding the numbers, so the PRD table and the code can never drift
//! silently out of sync.
//!
//! ## Real-vs-scaffolded status (read this before assuming a limit is enforced)
//!
//! Not every constant here has a real enforcement point wired up yet -- the
//! existing `control_channel.rs`/`holo_bridge/control.rs` code was built
//! before PRD 10.4 existed, and some of the limits below need wire-protocol
//! or session-state-machine surface this codebase does not have today (see
//! each constant's own doc comment for the specific gap, and
//! `holoiroh/README.md`'s "Session & rate limits (PRD 10.4)" section for the
//! consolidated table). Concretely, as of this module's introduction:
//!
//! - **Really enforced, wired into existing code:** [`AGENT_ACTION_CAP_DEFAULT`]
//!   via [`ActionCounter`], wired into
//!   [`crate::holo_bridge::control::HoloControlBridge`]'s per-turn dispatch.
//!   [`MAX_ACTIVE_TASKS_PER_MAC`] is already enforced by that same struct's
//!   pre-existing `busy`/`queue` mechanism (see that constant's doc).
//! - **Scaffolded (constant + helper type exist, real, independently
//!   exercised via `cargo run --example`) but not wired into a live call
//!   site**, because doing so needs machinery this codebase doesn't have
//!   yet (a task/session state machine, a wire-schema timestamp field, a
//!   manual-input channel): [`SESSION_LIFETIME_MAX_SECS`] /
//!   [`SessionTimer`], [`TASK_RUNTIME_DEFAULT_SECS`] /
//!   [`TASK_RUNTIME_MAX_SECS`] / [`clamp_task_runtime`],
//!   [`APPROVAL_TOKEN_TTL_SECS`] / [`ApprovalToken`],
//!   [`HEARTBEAT_INTERVAL_SECS`], [`DISCONNECT_PAUSE_SECS`] /
//!   [`DISCONNECT_CANCEL_SECS`], [`TASK_REQUEST_EXPIRY_SECS`],
//!   [`MANUAL_INPUT_RATE_MAX_PER_SEC`].
//! - **Investigated, real gap found and reported (not silently wired as if
//!   fixed):** [`MAX_ACTIVE_CONTROLLERS_PER_MAC`] -- see that constant's doc
//!   for the exact code path that currently does NOT reject a second
//!   simultaneous connection.
//!
//! Nothing in this module claims real enforcement it does not actually
//! have; see each constant/type's own doc for the precise story.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------
// Task request expiry
// ---------------------------------------------------------------------

/// Default expiry for a task request, in seconds, per PRD 10.4.
///
/// A task request older than this should be rejected rather than acted on
/// (a stale request that finally reaches the Mac after a long network
/// stall/reconnect should not silently execute).
///
/// **Not wired to a real enforcement point.** `crate::control_channel`'s
/// wire schema (`ClientMessage::Prompt`/`VoiceTranscript`, see
/// `holoiroh/PROTOCOL.md`) carries no timestamp field today, so there is no
/// `sent_at` to compute age against. Real enforcement requires a
/// wire-schema change (adding a `sent_at`/`expires_at` field to
/// `ClientMessage`), which is out of this module's scope -- see the
/// `holoiroh-task-envelope-protocol` PRD row, which already specifies an
/// `expires_at` field on the richer task-envelope schema PRD 7.1 defines.
#[allow(dead_code)]
pub const TASK_REQUEST_EXPIRY_SECS: u64 = 30;

// ---------------------------------------------------------------------
// Session lifetime
// ---------------------------------------------------------------------

/// Maximum lifetime of one active session, in seconds (10 minutes), per PRD
/// 10.4. A session reaching this age must end regardless of whether a task
/// is in flight.
///
/// Scaffolded via [`SessionTimer`], independently exercised (see
/// `examples/limits_probe.rs`), but not called from a live session-tracking
/// call site -- this codebase has no persistent "session" object spanning
/// multiple control-channel connections/tasks yet (each accepted `iroh`
/// connection in `control_channel.rs` is handled inline in
/// [`crate::control_channel::ControlChannel::accept`] with no session
/// struct threaded through it).
#[allow(dead_code)]
pub const SESSION_LIFETIME_MAX_SECS: u64 = 10 * 60;

/// Tracks the age of one active session against [`SESSION_LIFETIME_MAX_SECS`].
///
/// Real, independently-usable timer type (not a stub): construct with
/// [`SessionTimer::start`], and call [`SessionTimer::is_expired`] /
/// [`SessionTimer::remaining`] at any point to check the session's age
/// against the PRD 10.4 bound. See this module's doc for why nothing in
/// `control_channel.rs` calls this yet.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct SessionTimer {
    started_at: Instant,
    max: Duration,
}

#[allow(dead_code)]
impl SessionTimer {
    /// Starts a new session timer with the default PRD 10.4 lifetime
    /// ([`SESSION_LIFETIME_MAX_SECS`]).
    pub fn start() -> Self {
        Self::start_with_max(Duration::from_secs(SESSION_LIFETIME_MAX_SECS))
    }

    /// Starts a new session timer with an explicit max lifetime -- mainly
    /// for callers (and `examples/limits_probe.rs`) that need a short
    /// duration to observe real expiry without actually waiting 10 minutes.
    pub fn start_with_max(max: Duration) -> Self {
        Self {
            started_at: Instant::now(),
            max,
        }
    }

    /// True once this session has been alive at least `max` (default: PRD
    /// 10.4's 10 minutes).
    pub fn is_expired(&self) -> bool {
        self.started_at.elapsed() >= self.max
    }

    /// Time remaining before expiry, or `Duration::ZERO` if already expired
    /// (never negative/wrapping).
    pub fn remaining(&self) -> Duration {
        self.max.saturating_sub(self.started_at.elapsed())
    }
}

// ---------------------------------------------------------------------
// Approval token
// ---------------------------------------------------------------------

/// TTL for an approval token, in seconds, per PRD 10.4. An approval token
/// authorizes exactly **one task and one action** -- it is a single-use,
/// single-task-scoped credential, not a session-wide grant.
///
/// Scaffolded via [`ApprovalToken`], independently exercised, but not wired
/// into a live call site -- this codebase has no approval-gating flow yet
/// (see `holoiroh-sensitive-app-approval-gating` / `holoiroh-p014-*` PRD
/// rows for where a real approval flow would issue/consume these tokens).
#[allow(dead_code)]
pub const APPROVAL_TOKEN_TTL_SECS: u64 = 60;

/// A single-use, single-task-scoped approval credential per PRD 10.4:
/// valid for exactly one task and one action, and expires after
/// [`APPROVAL_TOKEN_TTL_SECS`] (60s) if unused.
///
/// Real, independently-usable type: [`ApprovalToken::issue`] mints one
/// scoped to a `task_id`; [`ApprovalToken::consume`] enforces both the TTL
/// and the single-use constraint atomically (a token can be consumed at
/// most once, and only before it expires).
#[allow(dead_code)]
#[derive(Debug)]
pub struct ApprovalToken {
    task_id: String,
    issued_at: Instant,
    ttl: Duration,
    consumed: std::sync::atomic::AtomicBool,
}

/// Why an [`ApprovalToken::consume`] call was rejected.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalTokenError {
    /// The token's TTL ([`APPROVAL_TOKEN_TTL_SECS`] by default) has elapsed.
    Expired,
    /// The token was already consumed by an earlier action -- one approval
    /// token authorizes exactly one action, per PRD 10.4.
    AlreadyConsumed,
    /// The token was issued for a different `task_id` than the one being
    /// consumed against -- one approval token authorizes exactly one task.
    WrongTask,
}

#[allow(dead_code)]
impl ApprovalToken {
    /// Issues a new approval token scoped to `task_id`, with the default
    /// PRD 10.4 TTL ([`APPROVAL_TOKEN_TTL_SECS`]).
    pub fn issue(task_id: impl Into<String>) -> Self {
        Self::issue_with_ttl(task_id, Duration::from_secs(APPROVAL_TOKEN_TTL_SECS))
    }

    /// Issues a new approval token with an explicit TTL -- mainly for
    /// `examples/limits_probe.rs` to observe real expiry without waiting
    /// 60s.
    pub fn issue_with_ttl(task_id: impl Into<String>, ttl: Duration) -> Self {
        Self {
            task_id: task_id.into(),
            issued_at: Instant::now(),
            ttl,
            consumed: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Attempts to consume this token to authorize one action within
    /// `task_id`. Succeeds at most once, only while unexpired, and only for
    /// the task it was issued for.
    ///
    /// Uses `compare_exchange` on the `consumed` flag so two concurrent
    /// `consume` calls racing on the same token can never both succeed --
    /// exactly the "one task + one action" guarantee PRD 10.4 requires.
    pub fn consume(&self, task_id: &str) -> Result<(), ApprovalTokenError> {
        if task_id != self.task_id {
            return Err(ApprovalTokenError::WrongTask);
        }
        if self.issued_at.elapsed() >= self.ttl {
            return Err(ApprovalTokenError::Expired);
        }
        match self.consumed.compare_exchange(
            false,
            true,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) => Ok(()),
            Err(_) => Err(ApprovalTokenError::AlreadyConsumed),
        }
    }

    /// True if this token could still be [`consume`](Self::consume)d right
    /// now (unexpired and not yet consumed) -- a non-mutating check, useful
    /// for UI-side "is this still valid" display without spending the
    /// token.
    pub fn is_valid(&self) -> bool {
        !self.consumed.load(Ordering::SeqCst) && self.issued_at.elapsed() < self.ttl
    }
}

// ---------------------------------------------------------------------
// Heartbeat
// ---------------------------------------------------------------------

/// Heartbeat interval while a session is active, in seconds, per PRD 10.4.
///
/// **Constant only, not wired to a real periodic sender/receiver.**
/// `crate::control_channel`'s wire schema
/// (`ClientMessage`/`ServerMessage`, see `holoiroh/PROTOCOL.md`) has no
/// heartbeat message variant today, so there is nothing to send/expect on
/// this interval yet. The natural insertion point for a real
/// heartbeat-send loop, once a wire message exists, is
/// [`crate::control_channel::ControlChannel::accept`]'s `tokio::select!`
/// (alongside the existing `lines.next_line()` / `send_task` arms) --
/// adding a `tokio::time::interval(Duration::from_secs(HEARTBEAT_INTERVAL_SECS))`
/// branch there would tick alongside the existing read/write arms without
/// disturbing them.
#[allow(dead_code)]
pub const HEARTBEAT_INTERVAL_SECS: u64 = 5;

// ---------------------------------------------------------------------
// Disconnect handling
// ---------------------------------------------------------------------

/// Seconds after a disconnect before a session is paused, per PRD 10.4.
///
/// **Constant only, not wired to a real timer.** See
/// [`DISCONNECT_CANCEL_SECS`]'s doc for where connection loss is actually
/// detected today and why starting a real pause timer from that point is
/// not yet reachable without a task/session state machine this codebase
/// doesn't have (there is currently nothing to "pause" -- an in-flight
/// `holo serve` A2A call has no pause primitive, only cancel; see
/// `crate::holo_bridge::control::HoloControlBridge::handle_stop`).
#[allow(dead_code)]
pub const DISCONNECT_PAUSE_SECS: u64 = 5;

/// Seconds after a disconnect before a session is canceled outright, unless
/// the in-flight task is "safely draft-complete" (i.e. has already reached
/// a state where canceling would lose nothing user-visible), per PRD 10.4.
///
/// **Constant only, not wired to a real timer.** Connection loss is
/// detected today in
/// [`crate::control_channel::ControlChannel::accept`]'s read loop: the
/// `Ok(None)` arm (peer closed cleanly) and the `Err(err)` arm (read error)
/// both simply `break` out of the loop and let the function return,
/// tearing the connection down immediately -- there is no pause-then-cancel
/// grace period today, and no "safely draft-complete" concept in the
/// current task model (`ControlEvent`/`ControlMessage` have no draft
/// state). Wiring a real disconnect timer needs both a task-state enum
/// (tracked separately as the `holoiroh-task-state-machine-terminal-statuses`
/// PRD row, which defines a `draft_ready` state that would be the natural
/// "safely draft-complete" signal) and a way to keep the in-flight
/// `HoloControlBridge` turn alive across a dropped `iroh` connection long
/// enough to pause/cancel it on a timer rather than tearing it down
/// synchronously with the accept loop's return.
#[allow(dead_code)]
pub const DISCONNECT_CANCEL_SECS: u64 = 15;

// ---------------------------------------------------------------------
// Concurrency caps
// ---------------------------------------------------------------------

/// Maximum number of tasks allowed to be actively running on one Mac at a
/// time, per PRD 10.4.
///
/// **Really enforced today**, not just scaffolded: this is already the
/// exact behavior of
/// [`crate::holo_bridge::control::HoloControlBridge`]'s pre-existing `busy`
/// flag + `queue` (a second `Prompt`/`VoiceTranscript` arriving while one
/// is in flight is queued, never run concurrently -- see that struct's own
/// doc comment, which now cites this constant explicitly). This constant
/// exists so that mechanism is traceable back to the PRD requirement it
/// satisfies, not left as an implicit "one at a time" design decision with
/// no named limit backing it.
pub const MAX_ACTIVE_TASKS_PER_MAC: usize = 1;

/// Maximum number of controllers (connected iOS clients actively driving
/// the Mac) allowed at a time, per PRD 10.4.
///
/// **Gap found and reported, not silently claimed as enforced.**
/// `holoiroh/README.md`'s "Control channel" section already notes that
/// `HoloBridge::start` takes a single `events_tx` at construction time, so
/// only one connection's events are ever delivered correctly -- but that is
/// a side effect of event routing, not an access-control check.
/// Concretely, in [`crate::control_channel::ControlChannel::accept`]:
/// every accepted `iroh` connection runs the *same* auth-gate-then-serve
/// logic independently, with no shared "is a controller already active"
/// guard anywhere in `ControlChannel`. A second simultaneous connection
/// from an already-allowlisted device is **not rejected** -- it passes
/// [`crate::control_channel::ControlChannel::authenticate`]'s fast path
/// (allowlisted device, no PIN needed) exactly like the first, gets its own
/// greeting, and calls
/// `self.bridge.replace_event_sink(events_tx.clone())` on every message it
/// sends, which silently steals the shared [`crate::holo_bridge::HoloBridge`]'s
/// single event sink out from under whichever connection had it before
/// (see `HoloControlBridge::replace_event_sink`'s own doc, which already
/// describes this exact reconnect-redirect mechanism -- it was designed for
/// "old connection dropped, new one takes over," not for "two connections
/// both alive, second one silently wins"). So today: two controllers CAN
/// coexist, the daemon does not reject the second, and only the most
/// recent sender's connection receives `ControlEvent`s -- the older
/// connection is not torn down, just silently starved of replies. This is
/// a real gap, not a full implementation of PRD 10.4's max-1-controller
/// requirement; see [`MAX_ACTIVE_CONTROLLERS_PER_MAC`]'s value and this
/// doc as the honest record of that gap. A real fix (tracked as a
/// follow-up rather than done here, since it changes accept-time rejection
/// behavior for an already-allowlisted device and needs product sign-off
/// on the resulting UX -- e.g. should the *new* or the *old* connection
/// win) would add an `Arc<Mutex<Option<String>>>` "current controller"
/// guard to `ControlChannel`, checked/set at the top of `accept` after
/// `authenticate` succeeds, rejecting a second device with a
/// `ServerMessage::error` before `accept_bi`'s stream is used for anything
/// else.
#[allow(dead_code)]
pub const MAX_ACTIVE_CONTROLLERS_PER_MAC: usize = 1;

// ---------------------------------------------------------------------
// Task runtime
// ---------------------------------------------------------------------

/// Default task runtime budget, in seconds, per PRD 10.4.
#[allow(dead_code)]
pub const TASK_RUNTIME_DEFAULT_SECS: u64 = 45;

/// Hard maximum task runtime, in seconds, per PRD 10.4. A caller may
/// request more than the default but never more than this.
#[allow(dead_code)]
pub const TASK_RUNTIME_MAX_SECS: u64 = 120;

/// Clamps a caller-requested task runtime to PRD 10.4's bounds: never above
/// [`TASK_RUNTIME_MAX_SECS`], and [`TASK_RUNTIME_DEFAULT_SECS`] when the
/// caller doesn't request an override.
///
/// Real, independently-usable function (not a stub) -- see
/// `examples/limits_probe.rs` for a live witness that an over-max request
/// is actually clamped, not passed through. Not yet called from
/// `holo_bridge::control` because that module has no per-task deadline
/// concept today (`HoloControlBridge::run_prompt` runs `send_and_stream` to
/// completion with no timeout at all); wiring a real deadline there is
/// tracked as a follow-up (would need `tokio::time::timeout` wrapping the
/// `send_and_stream` call, which changes error-handling/cancellation
/// behavior on timeout and deserves its own careful pass rather than a
/// drive-by wrap here).
#[allow(dead_code)]
pub fn clamp_task_runtime(requested: Option<Duration>) -> Duration {
    let max = Duration::from_secs(TASK_RUNTIME_MAX_SECS);
    match requested {
        Some(d) if d > max => max,
        Some(d) => d,
        None => Duration::from_secs(TASK_RUNTIME_DEFAULT_SECS),
    }
}

// ---------------------------------------------------------------------
// Agent action cap
// ---------------------------------------------------------------------

/// Default maximum number of agent actions allowed within a single task,
/// per PRD 10.4.
///
/// **Really enforced today**: see [`ActionCounter`], wired into
/// [`crate::holo_bridge::control::HoloControlBridge::run_prompt`] (one
/// counter constructed per task turn, incremented on every progress event
/// forwarded from `holo serve`, and the turn is aborted with a
/// [`crate::holo_bridge::control::ControlEvent::Error`] if the cap would be
/// exceeded).
/// 500, raised from the original 100: an A2A `Working` update is NOT one
/// agent action -- holo serve streams several status updates per real step
/// (observation, thought, tool call), and a live multi-app task on the
/// tinfoil fallback (kimi-k2-6) was witnessed emitting 100 updates within
/// 3.5 minutes of legitimate, non-looping work, latching the cap and (with
/// the old suppress-everything latch) swallowing the turn's real final
/// answer. 500 still bounds a genuinely runaway agent while giving real
/// tasks the headroom their event volume needs.
pub const AGENT_ACTION_CAP_DEFAULT: u32 = 500;

/// Counts agent actions within one task and refuses once
/// [`AGENT_ACTION_CAP_DEFAULT`] (or an explicit override) has been reached.
///
/// Uses an [`AtomicU32`] with `fetch_update` so concurrent callers can
/// never both observe "under the cap" and both succeed past it -- the same
/// race-safety discipline `HoloControlBridge`'s own `busy`/`queue` locking
/// already uses elsewhere in this crate.
#[derive(Debug)]
pub struct ActionCounter {
    count: AtomicU32,
    cap: u32,
}

impl Default for ActionCounter {
    /// A counter capped at [`AGENT_ACTION_CAP_DEFAULT`].
    fn default() -> Self {
        Self::new(AGENT_ACTION_CAP_DEFAULT)
    }
}

impl ActionCounter {
    /// A counter capped at [`AGENT_ACTION_CAP_DEFAULT`] (the PRD 10.4
    /// default for a task with no explicit override).
    pub fn new_default() -> Self {
        Self::default()
    }

    /// A counter capped at an explicit `cap` (for a task that has
    /// negotiated a different limit than the default -- PRD 10.4 names 100
    /// as the *default*, not a hardcoded universal ceiling).
    pub fn new(cap: u32) -> Self {
        Self {
            count: AtomicU32::new(0),
            cap,
        }
    }

    /// Records one more agent action. Returns `Ok(n)` with the new count
    /// (1-indexed) if under the cap, or `Err(cap)` if this action would
    /// exceed it -- the count is **not** incremented on a refused call, so
    /// a caller that keeps calling `try_record` after a refusal gets the
    /// same `Err` every time rather than drifting past the cap.
    pub fn try_record(&self) -> Result<u32, u32> {
        self.count
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                if current >= self.cap {
                    None
                } else {
                    Some(current + 1)
                }
            })
            .map(|prev| prev + 1)
            .map_err(|_| self.cap)
    }

    /// Current recorded action count.
    pub fn count(&self) -> u32 {
        self.count.load(Ordering::SeqCst)
    }

    /// The cap this counter enforces.
    pub fn cap(&self) -> u32 {
        self.cap
    }
}

// ---------------------------------------------------------------------
// Manual input rate
// ---------------------------------------------------------------------

/// Maximum manual (human, post-Take-Control) input event rate, in events
/// per second, per PRD 10.4.
///
/// **Constant only, no channel to attach it to yet.** This codebase's
/// current wire schema (`ClientMessage` in `crate::control_channel`, see
/// `holoiroh/PROTOCOL.md`) has exactly four variants -- `Prompt`,
/// `VoiceTranscript`, `Stop`, `Pin` -- all agent-directed or pairing
/// messages. There is no `manual_input` message type or stream at all
/// (the richer 6-stream protocol PRD 7.1 describes, which includes a
/// dedicated `manual_input` stream for post-Take-Control keyboard/mouse
/// events, is tracked separately under the
/// `holoiroh-task-envelope-protocol` PRD row and supersedes today's
/// `PROTOCOL.md`). Real rate-limiting needs that channel to exist first;
/// this constant is recorded now so the eventual implementation has an
/// authoritative value to import rather than re-deriving it from the PRD
/// text again.
#[allow(dead_code)]
pub const MANUAL_INPUT_RATE_MAX_PER_SEC: u32 = 120;
