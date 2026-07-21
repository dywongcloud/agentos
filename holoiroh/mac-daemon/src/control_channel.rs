//! Bidirectional control channel: carries small JSON messages between the
//! Mac daemon and the iOS app, alongside the `iroh-live` media broadcast,
//! and bridges them to [`crate::holo_bridge`].
//!
//! Schema is defined in `holoiroh/PROTOCOL.md`; keep the two in sync.
//!
//! ## Relationship to `holoiroh-wire`
//!
//! The literal wire-schema types this module used to define --
//! [`ClientMessage`]/[`ServerMessage`], [`TaskEnvelope<T>`],
//! [`CONTROL_ALPN`], [`write_line`]/[`read_line`], and
//! [`InboundEnvelopeState::validate_inbound`] -- now live in the
//! `holoiroh-wire` crate and are re-exported here (see that crate's module
//! doc for exactly why: `ios-bridge` needs them without pulling in this
//! crate's macOS-only `holo_bridge`/`audit_log` dependencies). This module
//! now owns only the *connection-handling logic that uses that schema*:
//! the `iroh` `ProtocolHandler` impl, the PIN/allowlist auth gate, the
//! `iroh::protocol::Router` wiring, per-connection outbound sequence state,
//! and the audit-log bookkeeping -- none of which is portable to iOS (or
//! is even meaningful there, since `ios-bridge` is the *client* side of
//! this protocol, not a second implementation of the daemon's connection
//! handling).
//!
//! ## Relationship to `holo_bridge::control`
//!
//! There are two distinct "control message" concepts in this crate, kept
//! deliberately separate:
//!
//! - **This module** (`control_channel`) owns the literal wire schema named
//!   in the task this module was built for: [`ClientMessage`] /
//!   [`ServerMessage`], exactly `{type, text?}` as documented in
//!   `PROTOCOL.md`, plus the actual `iroh` transport that carries them
//!   (ALPN registration, `accept_bi`/`open_bi`, NDJSON framing).
//! - [`crate::holo_bridge::control`] owns a richer *internal* schema
//!   (`ControlMessage` / `ControlEvent`, correlated by `request_id` and
//!   `context_id`) used to talk to the `holo serve` A2A bridge; it does not
//!   know about `iroh` or any wire framing at all (see its own module doc).
//!
//! [`ControlChannel`] is the seam between them: each accepted connection
//! decodes wire [`ClientMessage`]s, synthesizes a `request_id`, forwards a
//! translated [`crate::holo_bridge::control::ControlMessage`] into a
//! [`crate::holo_bridge::HoloBridge`], and translates the
//! [`crate::holo_bridge::control::ControlEvent`]s that come back into wire
//! [`ServerMessage`]s written back out on the same stream. This keeps
//! `holo_bridge` transport-agnostic (as its own docs intend) while giving
//! this module a real consumer instead of a dangling internal channel.
//!
//! ## Why a second ALPN, not a second stream multiplexed into the media
//! `Connection`
//!
//! `iroh`'s connection model is one `iroh::endpoint::Connection` per ALPN
//! (see `iroh::protocol::Router`, which dispatches an *incoming connection*
//! to a `ProtocolHandler` keyed by the negotiated ALPN -- it does not hand
//! out already-open connections to be shared across handlers). `iroh-live`
//! itself follows this exact pattern: `Live::register_protocols` mounts
//! `iroh_moq::ALPN` (media) and, when gossip is enabled, `iroh_gossip::ALPN`
//! as two *separate* ALPNs on the *same* `iroh::Endpoint` /
//! `iroh::protocol::Router` (see the vendored `iroh-live` source,
//! `iroh-live/src/live.rs::register_protocols`). This module mirrors that
//! idiom: `CONTROL_ALPN` is a third ALPN mounted on the same `Endpoint` via
//! [`ControlChannel::register_protocols`].
//!
//! This *is* "a second logical stream on the same iroh QUIC connection" in
//! the sense the surrounding architecture (see `holoiroh/README.md`) means
//! it: same `iroh::Endpoint`, same peer `EndpointId`, same NAT-punch/relay
//! path and connection-lifecycle/reconnect story as the media broadcast --
//! `iroh` just represents "a second logical stream to the same peer" as a
//! second `Connection` object over that shared transport rather than as a
//! stream nested inside the first `Connection`. Within that one control
//! `Connection`, the actual bidirectional data path is a single QUIC stream
//! opened with [`iroh::endpoint::Connection::open_bi`] (dial side) /
//! accepted with [`iroh::endpoint::Connection::accept_bi`] (accept side).

use std::sync::Arc;

use anyhow::{Context, Result};
use iroh::{
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler, RouterBuilder},
};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::allowlist::{Allowlist, verify_pin};
use crate::audit_log::{
    ActionClass, AppCategory, AuditEntry, AuditLogger, ConnectionPath, FinalStatus,
    InferenceMode, RemoteViewState, now_ms,
};
use crate::holo_bridge::{ControlEvent, ControlMessage, DoneStatus, HoloBridge, HoloControlBridge};

// Wire-protocol types/constants/framing helpers: these used to be defined
// in this module, but now live in `holoiroh-wire` (a pure-serde, no-iroh,
// no-macOS-dependency crate) so `ios-bridge` can depend on them without
// pulling in this crate's desktop-only `holo_bridge`/`audit_log` graph --
// see `holoiroh-wire/src/lib.rs`'s module doc for the full rationale.
// Re-exported (`pub use`) rather than only privately imported so existing
// external references to `control_channel::{ClientMessage, ServerMessage,
// TaskEnvelope, CONTROL_ALPN, ...}` (this crate's examples, PROTOCOL.md's
// prose) keep resolving at the same path.
// `#[allow(unused_imports)]`: several of these (`DEFAULT_EXPIRY_MS`, `EnvelopeRejection`,
// `InputRequestKind`, `PROTOCOL_VERSION`, `read_line`) are not referenced by name inside this
// module itself, only re-exported for external consumers (`examples/envelope_probe.rs`,
// `examples/input_request_probe.rs` import them as `holoiroh_daemon::control_channel::{...}`) --
// rustc's unused-import lint only sees intra-crate usage of a `pub use`, not downstream crates'
// imports of the re-exported path, so it flags these as unused even though removing them would
// break those examples' `use` statements.
#[allow(unused_imports)]
pub use holoiroh_wire::{
    CONTROL_ALPN, ClientMessage, DEFAULT_EXPIRY_MS, EnvelopeRejection, InboundEnvelopeState,
    InputRequestKind, PROTOCOL_VERSION, ServerMessage, TaskEnvelope, epoch_millis_now,
    input_request_expired_text, read_line, write_line,
};

/// Per-connection *outbound* envelope state: this connection's minted
/// `session_id` plus a monotonic counter for the daemon's own outbound
/// `sequence_number`s.
///
/// Owned entirely by [`ProtocolHandler::accept`]'s writer task (`send_task`)
/// -- kept separate from [`InboundEnvelopeState`] (rather than one combined
/// struct) because the two live in genuinely different places: the writer
/// task owns `send` for the connection's whole lifetime and needs this
/// state moved into it, while the read loop needs `InboundEnvelopeState`
/// mutably available on every line it reads. A single shared struct would
/// require the read loop and writer task to fight over one lock for two
/// logically-independent counters (inbound sequence tracking has nothing
/// to do with outbound sequence numbering -- see `next_outbound_sequence`'s
/// own doc).
pub struct OutboundEnvelopeState {
    pub session_id: String,
    next_outbound_sequence: u64,
}

impl OutboundEnvelopeState {
    /// Mints a fresh `session_id` (uuid v4) for a newly accepted
    /// connection.
    pub fn new() -> Self {
        OutboundEnvelopeState {
            session_id: uuid::Uuid::new_v4().to_string(),
            next_outbound_sequence: 0,
        }
    }

    /// Returns the next `sequence_number` to stamp on an outbound
    /// [`TaskEnvelope`] for this connection, advancing the counter.
    /// Independent of inbound sequence tracking -- the daemon's own
    /// outbound stream is numbered separately from whatever the peer
    /// sends, since the two are different logical sequences (matching the
    /// envelope being scoped per `session_id` per direction, not a single
    /// shared counter).
    pub fn next_outbound_sequence(&mut self) -> u64 {
        let n = self.next_outbound_sequence;
        self.next_outbound_sequence += 1;
        n
    }
}

impl Default for OutboundEnvelopeState {
    fn default() -> Self {
        Self::new()
    }
}

/// Translates a [`crate::holo_bridge::control::ControlEvent`] (the
/// internal, `request_id`/`context_id`-correlated bridge schema) down
/// to the minimal wire [`ServerMessage`] schema this module's
/// `PROTOCOL.md` defines. The correlation ids themselves are not part
/// of the wire schema (the task's literal ask has no such fields), so
/// they're folded into human-readable `text` rather than dropped
/// silently -- a future PROTOCOL.md revision may promote them to real
/// fields (see PROTOCOL.md's "Future extension" section).
///
/// A free function (rather than an inherent `impl ServerMessage` method,
/// which is how this was originally written) because `ServerMessage` now
/// lives in `holoiroh-wire` -- Rust's orphan rule forbids `impl`ing
/// inherent methods on a foreign type from this crate, and this function
/// specifically depends on `ControlEvent`/`DoneStatus`
/// (`crate::holo_bridge`'s internal, non-wire, desktop-side schema), which
/// is exactly the kind of dependency `holoiroh-wire` exists to keep out of
/// the wire-schema crate. Call sites (this module's own writer task, plus
/// `examples/control_channel_probe.rs`) now read
/// `control_channel::from_control_event(event)` instead of
/// `ServerMessage::from_control_event(event)` -- same behavior, different
/// call syntax.
///
/// ## Not wired to [`crate::task_state::TaskState`]
///
/// [`crate::task_state::TaskState`] is this crate's Project Aro PRD
/// task-lifecycle enum (created/queued/connecting/.../completed, plus
/// interactive-wait and terminal states) -- deliberately **not**
/// threaded into this function. The one variant here with any real
/// per-state correspondence, [`ControlEvent::Queued`] below, already
/// has a byte-exact wire string (`"queued, N ahead"`) asserted by
/// `examples/control_channel_probe.rs`; embedding `TaskState`'s
/// serialized value into it would be a breaking, unrequested wire
/// change, not a natural hook. Every other arm carries either free
/// text or the unrelated 3-way `DoneStatus`, with no correspondence to
/// `TaskState`'s finer granularity -- `holo_bridge::a2a_client`'s
/// `TaskUpdate` (`Working`/`Answer`/`Terminal`) is the actual upstream
/// event source, and it does not report which fine-grained lifecycle
/// state a task is in. The next task that gives this bridge a
/// fine-grained event source (e.g. `holo-desktop-cli` trajectory
/// events that name a specific step) is the one that should wire
/// `TaskState` in here, not this one.
pub fn from_control_event(event: ControlEvent) -> ServerMessage {
    match event {
        ControlEvent::Ack { .. } => ServerMessage::ack(),
        ControlEvent::Progress { text, .. } => {
            ServerMessage::task_progress(text.unwrap_or_default())
        }
        ControlEvent::Answer { text, .. } => ServerMessage::task_progress(text),
        ControlEvent::Done {
            status, message, ..
        } => {
            // Terminal lifecycle now reaches the wire as a first-class `task_done` frame
            // (additive `ServerMessage::TaskDone`) instead of being folded into a generic
            // `status`/`error` line: the phone's task controls (stop/pause/redirect UI)
            // need a reliable "this task ended, and how" signal to key off, which free
            // text never was. `status` carries the snake_case `DoneStatus` name, matching
            // its serde casing (`completed`/`failed`/`canceled`); the client styles
            // `failed` as an error row itself.
            let status_str = match status {
                DoneStatus::Completed => "completed",
                DoneStatus::Failed => "failed",
                DoneStatus::Canceled => "canceled",
            };
            ServerMessage::task_done(status_str, message)
        }
        ControlEvent::Error { message, .. } => ServerMessage::error(message),
        // Wire shape required verbatim: `{"type":"status","text":"queued, N ahead"}`.
        // `ahead == 0` still reads correctly ("queued, 0 ahead" = next to run once the
        // current turn finishes) rather than needing a separate zero-case message.
        ControlEvent::Queued { ahead, .. } => {
            ServerMessage::status(format!("queued, {ahead} ahead"))
        }
        ControlEvent::DaemonStatus { text } => ServerMessage::status(text),
        // The sensitive-app consent gate's ask, verbatim onto the wire's P0-14 shape.
        ControlEvent::InputRequested {
            request_id,
            kind,
            context,
            response_options,
            expires_at,
        } => ServerMessage::InputRequest {
            request_id,
            kind,
            context,
            response_options,
            expires_at,
        },
    }
}

/// The `request_id` a [`ControlEvent`] itself carries, when it names a real one -- used by
/// the writer task to stamp the correct envelope `task_id` on each outbound event. Before
/// turns were spawned off the read loop, the last-inbound-envelope's task_id was a safe
/// stand-in (one turn at a time, strictly request/response); with concurrent turns, an event
/// must correlate by its OWN id, else e.g. a mid-turn `Stop`'s inbound envelope would
/// re-stamp the still-streaming prompt's progress events with the stop's task_id.
fn event_request_id(event: &ControlEvent) -> Option<String> {
    let id = match event {
        ControlEvent::Ack { request_id }
        | ControlEvent::Progress { request_id, .. }
        | ControlEvent::Answer { request_id, .. }
        | ControlEvent::Done { request_id, .. }
        | ControlEvent::Error { request_id, .. }
        | ControlEvent::Queued { request_id, .. }
        | ControlEvent::InputRequested { request_id, .. } => request_id,
        ControlEvent::DaemonStatus { .. } => return None,
    };
    if id.is_empty() { None } else { Some(id.clone()) }
}

/// Converts a wire [`ClientMessage`] plus a synthesized `request_id` into
/// the internal [`ControlMessage`] shape [`crate::holo_bridge::HoloBridge`]
/// expects. The wire schema has no `context_id` (each `ClientMessage`
/// carries no session-continuity field per `PROTOCOL.md`), so every
/// message starts a fresh `holo serve` A2A context; per-connection
/// conversation continuity can be layered on later by threading a
/// connection-scoped `context_id` through here without any wire-format
/// change.
///
/// Returns `None` for [`ClientMessage::Pin`] -- that variant is consumed
/// entirely by [`ControlChannel::authenticate`]'s gate before the main
/// accept loop (below) ever calls this function; a `Pin` arriving mid-
/// stream (after auth already succeeded) has no `HoloBridge` equivalent to
/// translate to, so the accept loop acks it locally instead of forwarding
/// it (see the `Ok(ClientMessage::Pin { .. })` arm in
/// [`ProtocolHandler::accept`]).
///
/// Also returns `None` for [`ClientMessage::InputResponse`] -- that variant
/// answers a pending [`ServerMessage::InputRequest`] the accept loop itself
/// is tracking (matching `request_id` against the outstanding request and
/// clearing its expiry timer), not something `HoloBridge`'s A2A-oriented
/// `ControlMessage` has any equivalent shape for today.
///
/// `pub` (rather than private) specifically so `examples/holo_stop_probe.rs`
/// -- the run-by-hand witness for the remote kill-switch path (this repo's
/// no-unit-tests rule) -- can assert the exact wire-[`ClientMessage::Stop`]
/// -> internal-[`ControlMessage::Stop`] mapping directly, without spinning up
/// a live `iroh` connection to reach it through [`ProtocolHandler::accept`].
/// Still called internally by that accept loop, so it is not dead code from
/// the bin target's perspective.
pub fn to_control_message(request_id: String, msg: ClientMessage) -> Option<ControlMessage> {
    match msg {
        ClientMessage::Prompt { text } => Some(ControlMessage::Prompt {
            request_id,
            text,
            context_id: None,
        }),
        ClientMessage::VoiceTranscript { text } => Some(ControlMessage::VoiceTranscript {
            request_id,
            text,
            context_id: None,
            confidence: None,
        }),
        ClientMessage::Stop => Some(ControlMessage::Stop {
            request_id,
            context_id: None,
            force: false,
        }),
        ClientMessage::Pause => Some(ControlMessage::Pause { request_id }),
        ClientMessage::Resume => Some(ControlMessage::Resume { request_id }),
        ClientMessage::Redirect { text } => Some(ControlMessage::Redirect { request_id, text }),
        ClientMessage::Pin { .. } => None,
        ClientMessage::InputResponse { .. } => None,
    }
}

/// One [`ServerMessage::InputRequest`] a connection is currently waiting on
/// a [`ClientMessage::InputResponse`] for (or expiry of).
///
/// [`ControlChannel::accept`] tracks **at most one** of these per
/// connection at a time, matching `HoloControlBridge`'s existing
/// single-active-turn concurrency model (`busy`/`queue` in
/// `holo_bridge::control`) -- a turn that needs user input pauses that one
/// turn; it does not make sense for a single control-channel connection to
/// have multiple simultaneous outstanding input requests today, and
/// tracking more than one would need its own bounded-queue design this row
/// does not need to solve. A future multi-outstanding-request design would
/// replace this `Option` with a keyed map, but nothing in this daemon
/// currently produces more than one at a time.
///
/// Fields are private (constructed only by [`ControlChannel::accept`]'s
/// internal bookkeeping in real use); [`Self::for_probing`] is the one
/// exception, mirroring [`AuthState::for_probing`]'s rationale exactly --
/// `examples/input_request_probe.rs` needs to build one directly to witness
/// [`wait_for_expiry`]'s real timing behavior without spinning up a live
/// `iroh` connection.
pub struct PendingInputRequest {
    request_id: String,
    /// Epoch millis, same unit as [`ServerMessage::InputRequest::expires_at`]
    /// -- copied here (rather than re-deriving from a stored
    /// `ServerMessage`) so [`wait_for_expiry`] only needs this one `u64` to
    /// compute the sleep duration.
    expires_at: u64,
}

impl PendingInputRequest {
    /// Builds a `PendingInputRequest` directly for
    /// `examples/input_request_probe.rs` (see struct doc) -- not called
    /// from `main.rs`'s binary path, same status as
    /// [`AuthState::for_probing`].
    #[allow(dead_code)]
    pub fn for_probing(request_id: impl Into<String>, expires_at: u64) -> Self {
        PendingInputRequest {
            request_id: request_id.into(),
            expires_at,
        }
    }
}

/// Resolves once `pending`'s deadline (`expires_at`, epoch millis) has
/// passed, or never resolves at all if `pending` is `None` -- letting this
/// be used directly as one arm of `tokio::select!` in
/// [`ControlChannel::accept`]'s connection loop without that arm ever
/// firing spuriously when no request is outstanding.
///
/// Computes the sleep duration from *real* wall-clock time
/// ([`epoch_millis_now`]) on every poll rather than once up front, so a
/// deadline that is already in the past (or arrives while this future is
/// first constructed) resolves on the very next `.await` point instead of
/// via any special-cased branch -- `Duration::ZERO` sleeps resolve
/// immediately, which is exactly the desired "already expired -> safe-pause
/// right away" behavior for a degenerate past-`expires_at` request.
///
/// `pub` (rather than private) so `examples/input_request_probe.rs` can
/// race real `tokio::time` against a real [`PendingInputRequest`] the same
/// way [`ControlChannel::accept`]'s own `tokio::select!` does -- same
/// probe-access rationale as [`ControlChannel::authenticate`].
pub async fn wait_for_expiry(pending: &Option<PendingInputRequest>) {
    match pending {
        Some(p) => {
            let now = epoch_millis_now();
            let remaining = p.expires_at.saturating_sub(now);
            tokio::time::sleep(std::time::Duration::from_millis(remaining)).await;
        }
        None => std::future::pending::<()>().await,
    }
}

/// Audit-log start metadata for one in-flight task, recorded by the main [`ProtocolHandler::accept`]
/// loop at dispatch time and consumed by the `send_task` spawned in that same function when the
/// matching [`ControlEvent::Done`] arrives -- see the audit-log bookkeeping comment where
/// `audit_starts` is constructed in `accept` for why this is split across the two tasks.
struct AuditTaskStart {
    started_at_ms: u64,
    action_class: ActionClass,
}

/// Applies one [`ControlEvent`] to the running audit-log bookkeeping for its connection: tallies
/// `Progress` events into `action_counts`, and on `Done`, looks up (and removes) the matching
/// [`AuditTaskStart`] recorded at dispatch time to build and [`AuditLogger::append`] a complete
/// [`AuditEntry`] -- the "one entry when a task completes" half of this module's P0-12 wiring (the
/// "one entry when a task starts" half is the `audit_starts.lock()...insert(..)` call in `accept`
/// itself). A `request_id` with no matching `audit_starts` entry (i.e. not a `Prompt`/
/// `VoiceTranscript` -- see that insert's own doc for which `ClientMessage` kinds get a start
/// record) is silently skipped: nothing to close out, not an error.
///
/// Free function (rather than a `ControlChannel` method) because it needs to run inside
/// `send_task`'s spawned `async move` block, which does not hold `&self` -- taking every piece of
/// state it needs as an explicit parameter instead.
fn audit_on_control_event(
    audit: &AuditLogger,
    connection_path: &ConnectionPath,
    audit_starts: &Arc<std::sync::Mutex<std::collections::HashMap<String, AuditTaskStart>>>,
    action_counts: &mut std::collections::HashMap<String, u32>,
    event: &ControlEvent,
) {
    match event {
        ControlEvent::Progress { request_id, .. } => {
            *action_counts.entry(request_id.clone()).or_insert(0) += 1;
        }
        ControlEvent::Done {
            request_id, status, ..
        } => {
            let start = audit_starts
                .lock()
                .expect("audit_starts lock poisoned")
                .remove(request_id);
            let Some(start) = start else {
                // No start record (e.g. this `Done` closed out a queued prompt dropped by
                // `Stop`, or `Stop`'s own `Done` -- see `HoloControlBridge::handle_stop`; `Stop`
                // itself is intentionally not audit-started, see `accept`'s dispatch-time
                // comment) -- nothing to close out.
                return;
            };
            let action_count = action_counts.remove(request_id).unwrap_or(0);
            let completed_at_ms = now_ms();
            let entry = AuditEntry {
                task_id: request_id.clone(),
                started_at_ms: start.started_at_ms,
                completed_at_ms,
                app_category: AppCategory::Desktop,
                action_class: start.action_class,
                inference_mode: InferenceMode::Cloud,
                remote_view_state: RemoteViewState::Streaming,
                connection_path: *connection_path,
                final_status: FinalStatus::from(*status),
                latency_ms: completed_at_ms.saturating_sub(start.started_at_ms),
                action_count,
            };
            if let Err(err) = audit.append(&entry) {
                // Best-effort, matching `holo_bridge`'s own degrade-don't-crash posture (see
                // `main.rs`'s handling of `HoloBridge::start` failing): a disk/permissions
                // problem writing the audit log must never tear down the control-channel turn
                // that already completed successfully from the user's point of view.
                warn!(task_id = %request_id, error = %err, "audit log: failed to append entry");
            }
        }
        _ => {}
    }
}

/// Shared auth state consulted by [`ControlChannel::accept`]'s gate: the
/// persisted device allowlist plus the PIN generated for this daemon run.
///
/// Held behind `std::sync::Mutex` (not `tokio::sync::Mutex`): every access
/// is a short, synchronous critical section (a `HashSet`/`Vec` lookup, or a
/// JSON file write on the rare add-device path) with no `.await` inside the
/// lock, so a std lock is both correct and cheaper than an async one here --
/// the same reasoning `HoloControlBridge::events_tx` uses for its own
/// `std::sync::RwLock` (see that type's doc comment).
pub struct AuthState {
    allowlist: Allowlist,
    allowlist_path: std::path::PathBuf,
    /// The PIN this daemon process generated at startup. `None` means PIN
    /// auth is disabled for this run (see [`ControlChannel::new`] /
    /// [`ControlChannel::with_auth`]) -- every connection is then gated on
    /// the allowlist alone, and an unknown device is rejected outright with
    /// no PIN-entry path offered (matching "reject unknown/wrong-PIN
    /// connections": with no PIN configured, there is no correct PIN to
    /// enter, so unknown devices are simply rejected).
    expected_pin: Option<String>,
}

impl AuthState {
    /// Constructs an `AuthState` directly, bypassing the real `~/.holoiroh/allowlist.json`
    /// load `ControlChannel::new`/`with_auth` normally perform. `pub` (rather than only
    /// reachable via those constructors) specifically so `examples/auth_gate_probe.rs` -- a
    /// real, run-by-hand live witness for [`ControlChannel::authenticate`]'s PIN/allowlist gate
    /// logic (see this repo's no-unit-tests rule) -- can exercise the actual gate function
    /// against a real in-memory `AuthState` and a real `tokio::io::Lines` reader, the same seam
    /// the removed `#[tokio::test]` async tests used, just driven by `cargo run` instead of
    /// `cargo test`. Not called from `main.rs`'s binary target (which builds real `AuthState`
    /// only via `ControlChannel::new`/`with_auth`'s real allowlist load) -- `#[allow(dead_code)]`
    /// there, same status as `allowlist.rs`'s own probe-only convenience methods.
    #[allow(dead_code)]
    pub fn for_probing(expected_pin: Option<&str>, pre_allowed: &[&str], allowlist_path: std::path::PathBuf) -> Self {
        let mut allowlist = Allowlist::default();
        for device in pre_allowed {
            allowlist.add_entry(device.to_string(), None);
        }
        AuthState {
            allowlist,
            allowlist_path,
            expected_pin: expected_pin.map(|p| p.to_string()),
        }
    }

    /// True if `device_id` is currently allowlisted -- used by the probe to confirm
    /// `authenticate`'s side effect (adding a newly PIN-verified device) actually happened.
    /// Same not-called-from-`main.rs` status as [`Self::for_probing`].
    #[allow(dead_code)]
    pub fn contains_key(&self, device_id: &str) -> bool {
        self.allowlist.contains_key(device_id)
    }
}

/// Handle to the control channel: mounts [`CONTROL_ALPN`] on the shared
/// `iroh` `Endpoint`/`Router` (accept side) and lets the daemon open the
/// matching stream when dialing a peer (dial side).
///
/// Each accepted connection is first run through the auth gate documented
/// on [`ProtocolHandler::accept`] below (allowlist + first-connection PIN);
/// only a connection that passes is forwarded into the shared [`HoloBridge`]
/// and gets its [`crate::holo_bridge::control::ControlEvent`]s streamed back
/// out as [`ServerMessage`]s on the same stream.
#[derive(Clone)]
pub struct ControlChannel {
    bridge: Arc<HoloBridge>,
    auth: Arc<std::sync::Mutex<AuthState>>,
    /// Metadata-only local audit log (Project Aro PRD row P0-12) -- see `crate::audit_log`'s
    /// module doc for exactly what is and isn't recorded. `Arc` (not owned) because
    /// `ControlChannel` is itself cheaply `Clone`d per accepted connection (see this struct's own
    /// existing `bridge`/`auth` fields) and every clone must append to the same underlying file.
    audit: Arc<AuditLogger>,
}

impl std::fmt::Debug for ControlChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControlChannel").finish_non_exhaustive()
    }
}

impl ControlChannel {
    /// Creates a new control channel wrapping an already-started
    /// [`HoloBridge`], with **no auth enforced**: the allowlist is loaded
    /// from [`Allowlist::default_path`] (best-effort -- a load failure logs
    /// a warning and starts from an empty in-memory allowlist rather than
    /// failing daemon startup) but every device is treated as effectively
    /// allowlisted (no PIN, no rejection) until [`Self::with_auth`] is used
    /// instead. Kept as the zero-friction default for local dev/testing
    /// (matches this crate's existing "best-effort, degrade don't crash"
    /// posture -- see `main.rs`'s `holo_bridge` startup handling) --
    /// **not** what a real deployment should call; see `PAIRING.md`'s
    /// "Exact remaining wiring step" for what `main.rs` would need to
    /// change to actually enable enforcement by default.
    pub fn new(bridge: Arc<HoloBridge>, audit: Arc<AuditLogger>) -> Self {
        let (allowlist, allowlist_path) = Self::load_allowlist_best_effort();
        Self {
            bridge,
            auth: Arc::new(std::sync::Mutex::new(AuthState {
                allowlist,
                allowlist_path,
                expected_pin: None,
            })),
            audit,
        }
    }

    /// Creates a new control channel with auth **enforced**: `expected_pin`
    /// (typically [`crate::allowlist::generate_default_pin`]'s output,
    /// displayed to the user alongside the ticket/QR at startup) is
    /// required from any device not already in the persisted allowlist;
    /// devices that pass the PIN check are added to the allowlist and
    /// persisted immediately (so they don't need the PIN again on the next
    /// connection). This is the constructor `PAIRING.md` designs `main.rs`
    /// around, but `main.rs` does not call it yet -- see that file's
    /// "Exact remaining wiring step" section.
    pub fn with_auth(bridge: Arc<HoloBridge>, expected_pin: String, audit: Arc<AuditLogger>) -> Self {
        let (allowlist, allowlist_path) = Self::load_allowlist_best_effort();
        Self {
            bridge,
            auth: Arc::new(std::sync::Mutex::new(AuthState {
                allowlist,
                allowlist_path,
                expected_pin: Some(expected_pin),
            })),
            audit,
        }
    }

    fn load_allowlist_best_effort() -> (Allowlist, std::path::PathBuf) {
        match Allowlist::default_path() {
            Ok(path) => match Allowlist::load(&path) {
                Ok(list) => (list, path),
                Err(err) => {
                    warn!(error = %err, path = %path.display(), "control channel: failed to load allowlist, starting empty in-memory (not persisted until a successful pairing)");
                    (Allowlist::default(), path)
                }
            },
            Err(err) => {
                warn!(error = %err, "control channel: could not resolve allowlist path (HOME unset?), auth allowlist is in-memory-only this run");
                (Allowlist::default(), std::path::PathBuf::from(".holoiroh-allowlist-fallback.json"))
            }
        }
    }

    /// Runs the auth gate for a newly-accepted `connection`'s peer.
    ///
    /// Returns `Ok(())` if the connection may proceed (device was already
    /// allowlisted, or auth is disabled via [`Self::new`]). Returns
    /// `Err(reason)` if the connection must be rejected -- `reason` is
    /// meant to be sent back as a [`ServerMessage::auth_rejected`] before
    /// closing.
    ///
    /// For an unknown device with PIN auth enabled, this reads exactly one
    /// line off `lines` expecting `{"type":"pin","pin":"..."}` (the very
    /// first line the peer must send before anything else is processed --
    /// a `Prompt`/`VoiceTranscript`/`Stop` sent before a successful `Pin`
    /// from an unknown device is rejected, not queued or buffered). On a
    /// correct PIN, the device id is persisted to the allowlist immediately
    /// via [`Allowlist::save`] so future connections skip the PIN step.
    ///
    /// Free function taking `auth` explicitly (rather than a `&self`
    /// method reaching for `self.auth`) so it can be exercised directly --
    /// live, via `examples/auth_gate_probe.rs` (`cargo run --example
    /// auth_gate_probe`) -- without needing a real `Arc<HoloBridge>` (which
    /// requires a live `holo serve` subprocess) to construct a full
    /// `ControlChannel`. [`ControlChannel::accept`] simply calls
    /// `authenticate(&self.auth, ...)`. `pub` (rather than private) so that
    /// probe, a real run-by-hand live witness for this gate (see this
    /// repo's no-unit-tests rule), can call the actual function.
    pub async fn authenticate<R>(
        auth: &Arc<std::sync::Mutex<AuthState>>,
        remote: &str,
        lines: &mut tokio::io::Lines<R>,
    ) -> std::result::Result<(), String>
    where
        R: tokio::io::AsyncBufRead + Unpin,
    {
        // Fast path: already allowlisted (or PIN auth disabled entirely) --
        // no need to consume any input off the stream at all.
        {
            let state = auth.lock().expect("auth lock poisoned");
            if state.expected_pin.is_none() || state.allowlist.contains_key(remote) {
                return Ok(());
            }
        }

        // Unknown device, PIN auth enabled: the first line on the stream
        // must be a valid Pin message with the correct PIN.
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => return Err("connection closed before PIN was presented".to_string()),
            Err(err) => return Err(format!("read error waiting for PIN: {err}")),
        };

        let msg: ClientMessage = match serde_json::from_str(&line) {
            Ok(msg) => msg,
            Err(err) => return Err(format!("expected a PIN message first, got unparseable input: {err}")),
        };

        let candidate = match msg {
            ClientMessage::Pin { pin } => pin,
            other => {
                return Err(format!(
                    "expected a PIN message first from an unrecognized device, got {other:?} instead"
                ));
            }
        };

        let mut state = auth.lock().expect("auth lock poisoned");
        let expected = state
            .expected_pin
            .clone()
            .expect("checked Some above; not mutated between the two locks on this task-local path");

        if !verify_pin(&candidate, &expected) {
            return Err("incorrect PIN".to_string());
        }

        // Correct PIN: allowlist this device so it skips the PIN step on
        // every subsequent connection, and persist immediately -- a crash
        // between here and the next connection must not lose the pairing.
        state.allowlist.add_entry(remote.to_string(), None);
        if let Err(err) = state.allowlist.save(&state.allowlist_path) {
            // Persist failure doesn't revoke the in-memory grant for *this*
            // process's lifetime (the PIN was genuinely correct -- failing
            // the connection now would be punishing the user for a disk
            // error, not an auth failure), but it does mean the device will
            // have to re-enter the PIN after a daemon restart. Logged, not
            // silently swallowed.
            warn!(peer = %remote, error = %err, "control channel: PIN accepted but failed to persist allowlist -- device will need to re-pair after daemon restart");
        }
        info!(peer = %remote, "control channel: new device paired via PIN, added to allowlist");
        Ok(())
    }

    /// Mounts this control channel's [`ProtocolHandler`] onto `router`
    /// under [`CONTROL_ALPN`], alongside whatever other protocols (e.g.
    /// `iroh-live`'s MoQ/gossip via `Live::register_protocols`) are already
    /// registered on the same `Endpoint`. Mirrors
    /// `iroh_live::Live::register_protocols`'s own signature so the two
    /// compose in `main.rs` with the same builder-chaining pattern:
    ///
    /// ```ignore
    /// let router = live.register_protocols(RouterBuilder::new(endpoint));
    /// let router = control.register_protocols(router);
    /// let router = router.spawn();
    /// ```
    pub fn register_protocols(&self, router: RouterBuilder) -> RouterBuilder {
        router.accept(CONTROL_ALPN, self.clone())
    }

    /// Sends `msg` to a connected peer over a freshly opened bidirectional
    /// stream (dial side). Used when the Mac daemon needs to push a
    /// [`ServerMessage`] proactively rather than as a reply within an
    /// already-accepted stream (the common case, handled inline in
    /// [`ProtocolHandler::accept`] below). Not yet called from `main.rs`
    /// (nothing today needs to push a message outside of an active
    /// request/response turn), but it's the dial-side primitive this
    /// module exists to provide alongside the accept side.
    #[allow(dead_code)]
    pub async fn send_on_new_stream(conn: &Connection, msg: &ServerMessage) -> Result<()> {
        let (mut send, _recv) = conn.open_bi().await.context("opening control stream")?;
        write_line(&mut send, msg).await?;
        send.finish().context("finishing control stream")?;
        Ok(())
    }
}

impl ProtocolHandler for ControlChannel {
    async fn accept(&self, connection: Connection) -> std::result::Result<(), AcceptError> {
        let remote = connection.remote_id().fmt_short();
        info!(peer = %remote, "control channel: accepted connection");

        let (mut send, recv) = connection
            .accept_bi()
            .await
            .map_err(AcceptError::from_err)?;

        let mut lines = BufReader::new(recv).lines();

        // Auth gate: allowlisted devices pass through immediately; unknown
        // devices (with PIN auth enabled via `ControlChannel::with_auth`)
        // must present the correct PIN as their first line before anything
        // else on this stream is processed. See `authenticate`'s doc and
        // `holoiroh/PAIRING.md`'s "Auth beyond ticket possession" section.
        //
        // Deliberately NOT envelope-wrapped: no `session_id` exists yet at
        // this point (one is only minted, below, once auth succeeds), so
        // the PIN handshake stays a bare `ClientMessage::Pin` line/
        // `ServerMessage::auth_rejected` reply, same as before this task's
        // envelope wrapping. See `PROTOCOL.md`'s "Envelope" section for the
        // explicit statement of this boundary.
        if let Err(reason) = Self::authenticate(&self.auth, &remote.to_string(), &mut lines).await {
            warn!(peer = %remote, reason = %reason, "control channel: rejecting connection, auth failed");
            let _ = write_line(&mut send, &ServerMessage::auth_rejected(reason)).await;
            let _ = send.finish();
            connection.close(0u32.into(), b"auth rejected");
            return Ok(());
        }

        // Auth passed (or wasn't required): mint this connection's
        // session_id and outbound envelope state. See
        // `OutboundEnvelopeState`'s doc for why this is per-connection, not
        // persisted, and why it's a separate type from
        // `InboundEnvelopeState`.
        let mut outbound_state = OutboundEnvelopeState::new();
        let session_id = outbound_state.session_id.clone();
        info!(peer = %remote, session_id = %session_id, "control channel: session established");

        // Sends `msg` as a fresh envelope on this connection's outbound
        // sequence, optionally correlated to `task_id`. Centralizes the
        // envelope-construction boilerplate (fresh message_id, sent_at/
        // expires_at, this connection's next sequence_number) that would
        // otherwise be repeated at every one of this function's several
        // `ServerMessage` send sites.
        async fn send_envelope<W>(
            send: &mut W,
            outbound_state: &mut OutboundEnvelopeState,
            session_id: &str,
            task_id: Option<String>,
            msg: ServerMessage,
        ) -> Result<()>
        where
            W: tokio::io::AsyncWrite + Unpin,
        {
            let seq = outbound_state.next_outbound_sequence();
            let envelope =
                TaskEnvelope::<ServerMessage>::wrap(session_id.to_string(), task_id, seq, msg);
            // `write_line` returns `std::io::Result<()>` (moved to `holoiroh-wire`, which
            // deliberately has no `anyhow` dependency -- see that crate's doc comment on
            // `write_line`); `?` converts via `anyhow::Error: From<std::io::Error>` into this
            // function's own `anyhow::Result<()>`.
            write_line(send, &envelope).await?;
            Ok(())
        }

        // Metadata-only audit log (Project Aro PRD row P0-12, see `crate::audit_log`'s module
        // doc): the connection's direct-vs-relay path is determined once, here, from the live
        // `Connection` -- it cannot change for the lifetime of this accepted connection (a new
        // path renegotiation would be a new `Connection`), so every task audited on this
        // connection shares one `ConnectionPath` value rather than re-deriving it per task.
        let connection_path = ConnectionPath::from_connection(&connection);

        // Greet the peer so it knows the control channel is live -- this
        // also exercises the write path immediately, surfacing transport
        // errors early rather than only on the first real reply.
        if let Err(err) = send_envelope(
            &mut send,
            &mut outbound_state,
            &session_id,
            None,
            ServerMessage::status("control channel ready"),
        )
        .await
        {
            warn!(peer = %remote, error = %err, "control channel: failed to send greeting");
        }

        // Reconnect visibility: if a Holo task survived a previous connection's drop (still
        // running, or prompts still queued behind it -- see `HoloControlBridge::busy_state`),
        // tell the newly (re)connected peer immediately rather than leaving it to guess from
        // silence until the next `ControlEvent` happens to arrive. This is the direct fix for
        // "a stale in-flight Holo task should not be silently abandoned -- surface its
        // last-known state on reconnect".
        let (busy, queued) = self.bridge.busy_state();
        if busy || queued > 0 {
            let text = match (busy, queued) {
                (true, 0) => "reconnected: a Holo task is still running from before".to_string(),
                (true, n) => format!(
                    "reconnected: a Holo task is still running from before, {n} more queued behind it"
                ),
                (false, n) => format!("reconnected: {n} queued Holo task(s) waiting to run"),
            };
            if let Err(err) = send_envelope(
                &mut send,
                &mut outbound_state,
                &session_id,
                None,
                ServerMessage::status(text),
            )
            .await
            {
                warn!(peer = %remote, error = %err, "control channel: failed to send reconnect status");
            }
        }

        // Per-connection channel carrying translated ServerMessages back
        // from the HoloBridge (via ControlEvent) to this stream's writer.
        // `ControlEvent` alone (not a `(task_id, ControlEvent)` pair):
        // `HoloBridge::replace_event_sink` takes a plain
        // `mpsc::UnboundedSender<ControlEvent>` (see holo_bridge/mod.rs --
        // out of scope for this task's envelope wrapping, per the task's
        // "keep the existing ALPN/iroh transport code as-is" instruction,
        // which extends to not reshaping `holo_bridge`'s own transport-
        // agnostic sink type), so the task_id an outbound envelope should
        // echo is threaded via `current_task_id` below instead of through
        // the channel's element type.
        // Unbounded: ControlEvent volume is bounded by one holo_serve A2A
        // stream at a time per bridge (see holo_bridge::control's `emit`
        // doc), so this cannot grow unboundedly in practice, and using an
        // unbounded channel here avoids the bridge ever blocking on a slow
        // iroh peer.
        let (events_tx, mut events_rx) = mpsc::unbounded_channel::<ControlEvent>();

        // The task_id of the turn currently being driven through
        // `self.bridge.handle_message` (set by the read loop just before
        // each call, per the one-concurrent-turn-per-connection model this
        // module already has -- see the NOTE below). The writer task reads
        // this to stamp the correct `task_id` on each outbound envelope
        // translated from a `ControlEvent` without needing `HoloBridge`
        // itself to know anything about task_id/envelope concepts.
        let current_task_id: Arc<std::sync::Mutex<Option<String>>> =
            Arc::new(std::sync::Mutex::new(None));

        // Audit-log bookkeeping (Project Aro PRD row P0-12): `request_id` -> the task's start
        // metadata, recorded by the main accept loop below at the moment it dispatches a
        // `Prompt`/`VoiceTranscript` (the only point `ActionClass` is known -- `ControlEvent`
        // itself carries no action-class field), and consumed by `send_task` below when that
        // same `request_id`'s terminal `ControlEvent::Done` arrives. `std::sync::Mutex` (not
        // `tokio::sync::Mutex`): every critical section is a plain `HashMap` insert/remove with
        // no `.await` inside the lock, matching the same reasoning `AuthState`'s own doc comment
        // gives for its std lock.
        let audit_starts: Arc<std::sync::Mutex<std::collections::HashMap<String, AuditTaskStart>>> =
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));

        // Forward ControlEvents -> ServerMessage envelopes on this stream,
        // on its own task so a slow/stalled write to `send` doesn't block
        // the bridge from making progress on other connections' events (the
        // bridge itself is shared across all accepted connections).
        // `outbound_state` (`session_id` + the outbound sequence counter)
        // is moved into this task since it owns `send` for the rest of the
        // connection's lifetime; the read loop below owns its own, separate
        // `InboundEnvelopeState` instead (see that type's doc for why the
        // two are split) and never touches `send`/`outbound_state`
        // directly, only via `events_tx`.
        //
        // NOTE: because `events_tx` above is per-connection but `self.bridge`
        // is shared, in a multi-connection scenario every connection would
        // need its own bridge subscription to avoid cross-talk; today's
        // `HoloBridge::start` takes a single `events_tx` at construction
        // time (see holo_bridge::mod), so this daemon supports exactly one
        // concurrent control-channel connection driving the bridge, which
        // matches the one-Mac-one-iOS-client pairing model described in
        // README.md's security model. A future multi-client fan-out would
        // need `HoloBridge` to accept a per-request event sink instead.
        let mut send_task = tokio::spawn({
            let remote = remote.clone();
            let session_id = session_id.clone();
            let current_task_id = current_task_id.clone();
            let audit = self.audit.clone();
            let audit_starts = audit_starts.clone();
            async move {
                let mut outbound_state = outbound_state;
                // Per-connection action-step tally: incremented on every `Progress` event,
                // consumed (and removed) when that `request_id`'s `Done` arrives. Never touches
                // event content -- only counts how many `Progress` events were seen.
                let mut action_counts: std::collections::HashMap<String, u32> =
                    std::collections::HashMap::new();

                while let Some(event) = events_rx.recv().await {
                    audit_on_control_event(
                        &audit,
                        &connection_path,
                        &audit_starts,
                        &mut action_counts,
                        &event,
                    );
                    // Correlate by the event's OWN request_id first (concurrent turns are
                    // real now that the read loop spawns them); the last-inbound-envelope
                    // fallback only covers events that genuinely carry no id of their own.
                    let task_id = event_request_id(&event).or_else(|| {
                        current_task_id
                            .lock()
                            .expect("current_task_id lock poisoned")
                            .clone()
                    });
                    let msg = from_control_event(event);
                    if let Err(err) =
                        send_envelope(&mut send, &mut outbound_state, &session_id, task_id, msg)
                            .await
                    {
                        warn!(peer = %remote, error = %err, "control channel: failed to write event");
                        break;
                    }
                }
            }
        });

        // Inbound envelope-validation state (seen-set + last accepted
        // sequence_number), owned by this read loop -- see
        // `InboundEnvelopeState`'s doc for why it's a separate type/
        // instance from the writer task's `outbound_state` above.
        let mut inbound_state = InboundEnvelopeState::new();

        // The `request_id` of the single outstanding `InputRequest` this
        // connection is waiting on, if any -- see `PendingInputRequest`'s
        // doc for why this daemon tracks at most one at a time. `None` most
        // of the time; only `Some` between the moment an `InputRequest` is
        // sent (see `HoloControlBridge`/future callers of
        // `ServerMessage::input_request`) and either a matching
        // `InputResponse` or the expiry timer firing.
        let mut pending_input_request: Option<PendingInputRequest> = None;

        loop {
            // Race the next inbound line against both the writer task ending
            // (existing behavior) and, when a request is outstanding, its
            // expiry deadline -- `tokio::time::sleep_until` on a `None`
            // pending request would never fire could not be expressed
            // directly in `select!`, so the sleep future itself is only
            // constructed/polled when `pending_input_request` is `Some`
            // (`Either`-free via a plain `match` producing a boxed future
            // would work too, but a local async block capturing the
            // `Option` by reference and immediately returning if `None` is
            // simpler and allocation-free).
            let line = tokio::select! {
                line = lines.next_line() => line,
                _ = &mut send_task => {
                    debug!(peer = %remote, "control channel: writer task ended");
                    break;
                }
                _ = wait_for_expiry(&pending_input_request) => {
                    // `wait_for_expiry` only resolves when
                    // `pending_input_request` is `Some` and its deadline has
                    // passed -- safe to `.take()` and `.expect()` here.
                    let expired = pending_input_request.take().expect(
                        "wait_for_expiry only resolves when pending_input_request is Some",
                    );
                    warn!(
                        peer = %remote,
                        request_id = %expired.request_id,
                        "control channel: input_request expired with no response, pausing safely"
                    );
                    // Routed through `events_tx` (like every other outgoing
                    // message on this connection) rather than writing to
                    // `send` directly -- `send` was moved into `send_task`
                    // above, and `ControlEvent::DaemonStatus` is exactly the
                    // "out-of-band, not tied to a request/response turn"
                    // shape this is (see that variant's doc). It maps to a
                    // `ServerMessage::Status` (never `Error`) via
                    // `ServerMessage::from_control_event`, matching the
                    // "safely paused, not failed" requirement.
                    if events_tx
                        .send(ControlEvent::DaemonStatus {
                            text: input_request_expired_text(&expired.request_id),
                        })
                        .is_err()
                    {
                        break;
                    }
                    continue;
                }
            };

            let line = match line {
                Ok(Some(line)) => line,
                Ok(None) => {
                    debug!(peer = %remote, "control channel: peer closed stream");
                    break;
                }
                Err(err) => {
                    warn!(peer = %remote, error = %err, "control channel: read error");
                    break;
                }
            };

            if line.trim().is_empty() {
                continue;
            }

            // Two-stage parse: first the envelope shell, then its
            // `payload` field as a `ClientMessage`. Kept as two explicit
            // `serde_json` calls (rather than one `TaskEnvelope<
            // ClientMessage>` deserialize) so a malformed *envelope*
            // (missing/wrong-typed framing fields) and a well-formed
            // envelope wrapping a malformed/unknown-type *payload* produce
            // distinguishable error text -- both stay non-fatal per
            // PROTOCOL.md's existing malformed-input contract either way.
            let envelope_value: std::result::Result<serde_json::Value, _> =
                serde_json::from_str(&line);
            let envelope_value = match envelope_value {
                Ok(v) => v,
                Err(parse_err) => {
                    warn!(peer = %remote, error = %parse_err, line = %line, "control channel: malformed envelope JSON");
                    *current_task_id.lock().expect("current_task_id lock poisoned") = None;
                    if events_tx
                        .send(ControlEvent::Error {
                            request_id: String::new(),
                            message: format!("malformed envelope: {parse_err}"),
                        })
                        .is_err()
                    {
                        debug!(peer = %remote, "control channel: writer task gone, dropping parse-error reply");
                        break;
                    }
                    continue;
                }
            };

            let envelope: TaskEnvelope<serde_json::Value> =
                match serde_json::from_value(envelope_value) {
                    Ok(env) => env,
                    Err(shape_err) => {
                        warn!(peer = %remote, error = %shape_err, line = %line, "control channel: envelope missing required framing fields");
                        *current_task_id.lock().expect("current_task_id lock poisoned") = None;
                        if events_tx
                            .send(ControlEvent::Error {
                                request_id: String::new(),
                                message: format!("malformed envelope: {shape_err}"),
                            })
                            .is_err()
                        {
                            debug!(peer = %remote, "control channel: writer task gone, dropping parse-error reply");
                            break;
                        }
                        continue;
                    }
                };

            // Every reply this iteration echoes this envelope's task_id
            // (possibly `None`) -- set once here so the writer task picks
            // it up regardless of which arm below actually replies.
            *current_task_id.lock().expect("current_task_id lock poisoned") =
                envelope.task_id.clone();

            // Envelope shell parsed. Validate expiry/dedup/sequence before
            // touching the payload at all -- a rejected envelope is never
            // forwarded to holo_bridge regardless of what its payload
            // contains.
            if let Err(rejection) = inbound_state.validate_inbound(&envelope) {
                warn!(peer = %remote, %rejection, message_id = %envelope.message_id, "control channel: envelope rejected");
                if events_tx
                    .send(ControlEvent::Error {
                        request_id: String::new(),
                        message: format!("envelope rejected: {rejection}"),
                    })
                    .is_err()
                {
                    debug!(peer = %remote, "control channel: writer task gone, dropping rejection reply");
                    break;
                }
                continue;
            }

            // Envelope accepted: now parse `payload` as the actual
            // ClientMessage. A well-formed envelope wrapping a malformed/
            // unknown-type payload is reported distinctly from the
            // envelope-shape failures above.
            match serde_json::from_value::<ClientMessage>(envelope.payload) {
                Ok(ClientMessage::Pin { .. }) => {
                    // A Pin sent after auth already passed (e.g. an already-
                    // allowlisted device, or a second Pin from a device that
                    // just paired) is redundant, not an error -- ack it and
                    // keep reading rather than tearing down the connection.
                    debug!(peer = %remote, "control channel: redundant Pin message after auth, acking");
                    if events_tx
                        .send(ControlEvent::Ack { request_id: String::new() })
                        .is_err()
                    {
                        break;
                    }
                }
                Ok(ClientMessage::InputResponse { request_id, selected_option }) => {
                    // Sensitive-app consent decisions are resolved by the bridge itself
                    // (it owns the paused turn + allowance state); everything else falls
                    // through to this connection's generic pending-input tracking.
                    if HoloControlBridge::resolve_consent(&self.bridge, &request_id, &selected_option) {
                        debug!(
                            peer = %remote,
                            request_id = %request_id,
                            selected_option = %selected_option,
                            "control channel: sensitive-app consent resolved"
                        );
                        if events_tx
                            .send(ControlEvent::Ack {
                                request_id: request_id.clone(),
                            })
                            .is_err()
                        {
                            break;
                        }
                        continue;
                    }
                    match &pending_input_request {
                        Some(pending) if pending.request_id == request_id => {
                            debug!(
                                peer = %remote,
                                request_id = %request_id,
                                selected_option = %selected_option,
                                "control channel: input_request answered"
                            );
                            pending_input_request = None;
                            if events_tx
                                .send(ControlEvent::Ack {
                                    request_id: request_id.clone(),
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        _ => {
                            // Per PROTOCOL.md's malformed-input philosophy:
                            // an InputResponse that doesn't match anything
                            // outstanding (already expired, already
                            // answered, or never sent) is not a transport
                            // error -- reply with a normal error event and
                            // keep the connection open.
                            warn!(
                                peer = %remote,
                                request_id = %request_id,
                                "control channel: input_response for no matching pending input_request (already expired or unknown), ignoring"
                            );
                            if events_tx
                                .send(ControlEvent::Error {
                                    request_id: request_id.clone(),
                                    message: "no matching pending input_request (already expired or unknown)".to_string(),
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                }
                Ok(msg) => {
                    debug!(peer = %remote, ?msg, "control channel: received message");
                    // task_id threading: an inbound envelope that already
                    // names a task_id reuses it as the bridge's
                    // request_id (continuing/correlating with that task);
                    // an envelope with no task_id (e.g. a client that
                    // doesn't yet track one) gets a fresh uuid synthesized,
                    // same as this daemon did before envelope-wrapping --
                    // and that freshly-synthesized id becomes the task_id
                    // this turn's replies echo, so the writer task's
                    // outbound envelopes still correlate correctly even
                    // when the inbound envelope itself omitted task_id.
                    let request_id = envelope
                        .task_id
                        .clone()
                        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                    *current_task_id.lock().expect("current_task_id lock poisoned") =
                        Some(request_id.clone());

                    // Audit-log task start (Project Aro PRD row P0-12): only `Prompt`/
                    // `VoiceTranscript` are tasks with a start/end lifecycle worth auditing --
                    // `Stop` has no `Done` terminal event of its own kind to close the loop on
                    // (see `HoloControlBridge::handle_stop`, which emits `Done{Canceled}` for
                    // dropped queued prompts using *their own* `request_id`s, not `Stop`'s), so
                    // it is intentionally not given a start record here. `Pin`/`InputResponse`
                    // are handled entirely by their own arms above and never actually reach this
                    // match (kept as an explicit `None` arm so the match stays exhaustive over
                    // `ClientMessage`). Recorded from `msg` itself, before it's consumed into
                    // `to_control_message` below -- this is the only point in this whole turn
                    // where `ActionClass` (which wire message kind arrived) is known;
                    // `ControlEvent`/`ControlMessage` never carry it.
                    if let Some(action_class) = match &msg {
                        ClientMessage::Prompt { .. } => Some(ActionClass::Prompt),
                        ClientMessage::VoiceTranscript { .. } => Some(ActionClass::VoiceTranscript),
                        // Redirect and Resume both start a real turn whose eventual `Done`
                        // closes the audit entry, exactly like a prompt -- audited under the
                        // Prompt class rather than growing the on-disk audit schema for what
                        // is semantically "a prompt that replaced/continued another".
                        ClientMessage::Redirect { .. } | ClientMessage::Resume => {
                            Some(ActionClass::Prompt)
                        }
                        // Pause has no terminal of its own (the paused turn's cancel closes
                        // the original entry), same rationale as Stop.
                        ClientMessage::Stop
                        | ClientMessage::Pause
                        | ClientMessage::Pin { .. }
                        | ClientMessage::InputResponse { .. } => None,
                    } {
                        audit_starts.lock().expect("audit_starts lock poisoned").insert(
                            request_id.clone(),
                            AuditTaskStart {
                                started_at_ms: now_ms(),
                                action_class,
                            },
                        );
                    }
                    // Only `Pin`/`InputResponse` map to `None`, and both are
                    // handled entirely by the arms above -- every other
                    // `ClientMessage` variant always produces `Some`.
                    let Some(control_message) = to_control_message(request_id, msg) else {
                        continue;
                    };
                    // Register this connection's event sink, then SPAWN the handling
                    // rather than awaiting it inline. The old inline `.await` was the
                    // root cause of "stop can't stop a running task": a prompt turn
                    // streams for its whole lifetime inside `handle_message`, and this
                    // read loop -- the only reader of the stream -- was parked inside
                    // that await, so a mid-turn `Stop`/`Pause`/`Redirect` line sat
                    // unread in the QUIC buffer until the turn it was meant to
                    // interrupt had already finished. The bridge's own `busy`/`queue`
                    // discipline (built for exactly this concurrency) serializes the
                    // actual A2A turns; control verbs now process immediately.
                    self.bridge.replace_event_sink(events_tx.clone());
                    let bridge = self.bridge.clone();
                    tokio::spawn(async move {
                        bridge.handle_message(control_message).await;
                    });
                }
                Err(parse_err) => {
                    warn!(peer = %remote, error = %parse_err, line = %line, "control channel: malformed payload");
                    if events_tx
                        .send(ControlEvent::Error {
                            request_id: String::new(),
                            message: format!("malformed payload: {parse_err}"),
                        })
                        .is_err()
                    {
                        debug!(peer = %remote, "control channel: writer task gone, dropping parse-error reply");
                        break;
                    }
                    // Per PROTOCOL.md: malformed input is not a transport
                    // error. Keep reading.
                }
            }
        }

        drop(events_tx);
        let _ = send_task.await;
        connection.closed().await;
        Ok(())
    }
}

/// Shared handle type, for call sites that want to clone-and-store the
/// channel behind an `Arc` rather than relying on `ControlChannel`'s own
/// `Clone` (which is cheap -- it only clones an `Arc<HoloBridge>`). Not
/// used by `main.rs` today (it clones `ControlChannel` directly), kept as
/// a documented convenience alias for callers that prefer an explicit
/// `Arc` (e.g. storing it in a struct field alongside other `Arc`-wrapped
/// daemon state).
#[allow(dead_code)]
pub type SharedControlChannel = Arc<ControlChannel>;
