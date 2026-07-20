//! Pure-serde wire-protocol types shared by `holoiroh-daemon` (`mac-daemon`)
//! and `holoiroh-ios-bridge` (`ios-bridge`).
//!
//! ## Why this crate exists
//!
//! `ios-bridge` is the FFI crate the SwiftUI app links against, and it must
//! cross-compile to `aarch64-apple-ios`. `mac-daemon`'s `control_channel`
//! module -- where these types used to live -- pulls in `holo_bridge` and
//! `audit_log`, which transitively depend on macOS-only APIs (
//! `ScreenCaptureKit` and friends via `iroh-live`'s capture backend, TCC
//! preflight via `objc2-application-services`, etc.) that simply do not
//! exist on iOS. That meant `ios-bridge` could never depend on
//! `holoiroh-daemon` directly, so it grew its own small duplicate of the
//! wire surface it actually needs (starting with the `CONTROL_ALPN` byte
//! string) instead of sharing one definition.
//!
//! This crate is the fix: every type in here is plain `serde`-derived data
//! plus the two constants and two NDJSON framing helpers the wire protocol
//! is built from -- nothing here touches `iroh`, `tokio`'s runtime, or any
//! macOS-specific API, so it cross-compiles to `aarch64-apple-ios` cleanly.
//! `mac-daemon`'s `control_channel` module now imports these types rather
//! than defining them, and `ios-bridge` depends on this crate directly
//! instead of hand-duplicating `CONTROL_ALPN` (and can adopt the typed
//! `ClientMessage`/`ServerMessage`/`TaskEnvelope<T>` structs directly, in
//! Rust, in the future, instead of only ever seeing them as opaque JSON
//! strings crossing the FFI boundary, which is what it does today).
//!
//! What deliberately did **not** move here: anything that is connection-
//! handling logic rather than wire schema -- the `iroh` `ProtocolHandler`
//! impl, the auth gate (`AuthState`/`ControlChannel::authenticate`), the
//! `iroh::protocol::Router` wiring, and the per-connection
//! `OutboundEnvelopeState`/audit-log bookkeeping all stay in
//! `mac-daemon/src/control_channel.rs`, since that logic *uses* this wire
//! schema rather than being part of it, and (for the `iroh`/audit-log
//! pieces specifically) is exactly the kind of desktop-coupled code this
//! split exists to keep out of `ios-bridge`'s dependency graph.
//! `ServerMessage::from_control_event` also stays in `mac-daemon` as a free
//! function (rather than an inherent method on the now-foreign
//! `ServerMessage` type) for the same reason: it translates from
//! `holo_bridge::ControlEvent`/`DoneStatus`, an internal, non-wire,
//! desktop-side schema.
//!
//! Wire schema: see `holoiroh/PROTOCOL.md` for the authoritative
//! human-readable description; keep this file and that document in sync.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

/// ALPN identifying the control-channel protocol on the shared `iroh`
/// `Endpoint`. Follows the same app-specific-ALPN convention as
/// `iroh_moq::ALPN` / `iroh_gossip::ALPN`.
pub const CONTROL_ALPN: &[u8] = b"holoiroh/control/1";

/// This daemon's implementation of the Project Aro PRD's task-envelope
/// schema, as it currently exists. Bumped only on a deliberate,
/// coordinated wire-format change (see `PROTOCOL.md`'s "Envelope
/// versioning" section) -- not tied to the crate's own `Cargo.toml`
/// version.
pub const PROTOCOL_VERSION: u32 = 1;

/// Default lifetime of an envelope, in milliseconds, when a message is
/// constructed via [`TaskEnvelope::new`] without an explicit
/// `expires_at`: `sent_at + DEFAULT_EXPIRY_MS`. Matches the task's
/// specified default expiry window (30s).
pub const DEFAULT_EXPIRY_MS: u64 = 30_000;

/// Returns the current wall-clock time as Unix-epoch milliseconds.
///
/// `u64` (not `i64`): matches this crate's existing timestamp convention
/// (`allowlist.rs`'s `AllowlistEntry::paired_at: u64`, in seconds) scaled
/// up to millisecond precision -- the envelope's 30s-default expiry window
/// needs sub-second granularity that a seconds-resolution timestamp
/// can't express cleanly (e.g. "expires 30500ms after sent_at").
fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The Project Aro PRD's authoritative task-envelope shape, wrapping this
/// module's existing [`ClientMessage`]/[`ServerMessage`] wire payloads.
///
/// Generic over the payload type `T` so the same envelope shape serializes
/// both directions: `TaskEnvelope<ClientMessage>` (iOS -> Mac) and
/// `TaskEnvelope<ServerMessage>` (Mac -> iOS). See `PROTOCOL.md`'s
/// "Envelope" section for the authoritative field-by-field description and
/// wire examples.
///
/// ## Field notes
///
/// - `message_type`: mirrors the payload's own internally-tagged `type`
///   discriminant (e.g. `"prompt"`, `"ack"`) as an envelope-level field,
///   set by [`TaskEnvelope::new`] from the payload it's given -- this is
///   deliberately redundant with `payload`'s own serde tag (rather than
///   the two being unified into one tag) so the envelope's framing fields
///   are fully inspectable without deserializing into the payload's
///   concrete type first, matching the PRD's flat envelope shape.
/// - `session_id`: minted once per accepted `iroh` connection (see
///   `OutboundEnvelopeState::new` in `mac-daemon/src/control_channel.rs`)
///   and stable for that connection's lifetime; included on every envelope
///   either direction and validated against on inbound envelopes (see
///   [`InboundEnvelopeState::validate_inbound`]).
/// - `task_id`: correlates an envelope with the `ControlMessage`/
///   `ControlEvent` turn it belongs to -- see `mac-daemon`'s
///   `to_control_message` and `from_control_event`'s call sites in its
///   `ProtocolHandler::accept` for exactly how it's threaded through.
///   `None` is valid for envelopes with no bridge-turn correlation (e.g.
///   the initial greeting, or a `stop` with no specific task to target).
/// - `sent_at` / `expires_at`: Unix-epoch milliseconds. An inbound
///   envelope with `now_unix_ms() > expires_at` is rejected -- see
///   [`InboundEnvelopeState::validate_inbound`].
/// - `signature`: present on the wire per the PRD schema, but **not
///   cryptographically verified this pass** -- this codebase has no
///   signing keypair/identity infrastructure yet (the `iroh` node keypair
///   authenticates the *transport*, not individual envelopes). Always
///   `None` on envelopes this daemon constructs; an inbound envelope's
///   `signature` (if any) is deserialized and carried but not checked.
///   See `PROTOCOL.md`'s "Known gaps" section -- this is a documented gap,
///   not a silent omission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEnvelope<T> {
    pub protocol_version: u32,
    pub message_id: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub task_id: Option<String>,
    pub message_type: String,
    pub sent_at: u64,
    pub expires_at: u64,
    pub sequence_number: u64,
    pub payload: T,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub signature: Option<String>,
}

impl<T> TaskEnvelope<T> {
    /// Builds a fresh envelope for `payload`, stamping `sent_at = now`,
    /// `expires_at = now + DEFAULT_EXPIRY_MS`, a fresh `message_id`, and
    /// `signature: None` (see the struct doc's note on signing).
    /// `sequence_number` is supplied by the caller rather than derived
    /// here, since it's connection-scoped state this free function has no
    /// access to -- see `OutboundEnvelopeState::next_outbound_sequence` in
    /// `mac-daemon/src/control_channel.rs`.
    pub fn new(
        session_id: String,
        task_id: Option<String>,
        message_type: impl Into<String>,
        sequence_number: u64,
        payload: T,
    ) -> Self {
        let sent_at = now_unix_ms();
        TaskEnvelope {
            protocol_version: PROTOCOL_VERSION,
            message_id: uuid::Uuid::new_v4().to_string(),
            session_id,
            task_id,
            message_type: message_type.into(),
            sent_at,
            expires_at: sent_at + DEFAULT_EXPIRY_MS,
            sequence_number,
            payload,
            signature: None,
        }
    }

    /// True if `now` is past this envelope's `expires_at` -- the exact
    /// check [`InboundEnvelopeState::validate_inbound`] applies to inbound
    /// envelopes, exposed standalone so it's directly unit-witnessable
    /// (see `mac-daemon/examples/envelope_probe.rs`) without needing a full
    /// `InboundEnvelopeState`.
    pub fn is_expired_at(&self, now: u64) -> bool {
        now > self.expires_at
    }
}

impl TaskEnvelope<ClientMessage> {
    /// Convenience constructor deriving `message_type` from `payload`'s own
    /// discriminant, so call sites don't have to keep a separate string
    /// literal in sync with the `ClientMessage` variant they're wrapping.
    /// Not called from the daemon's own bin target -- this daemon only
    /// ever *receives* envelope-wrapped `ClientMessage`s (from a real iOS
    /// client), never constructs one itself -- but it's the dial-side
    /// primitive a real client implementation (or a probe simulating one,
    /// see `mac-daemon/examples/control_probe.rs`/
    /// `mac-daemon/examples/control_channel_probe.rs`) needs, same status
    /// as this module's other probe-only convenience methods (e.g.
    /// `AuthState::for_probing`).
    #[allow(dead_code)]
    pub fn wrap(
        session_id: String,
        task_id: Option<String>,
        sequence_number: u64,
        payload: ClientMessage,
    ) -> Self {
        let message_type = payload.type_tag();
        TaskEnvelope::new(session_id, task_id, message_type, sequence_number, payload)
    }
}

impl TaskEnvelope<ServerMessage> {
    /// Convenience constructor deriving `message_type` from `payload`'s own
    /// discriminant. See [`TaskEnvelope::<ClientMessage>::wrap`].
    pub fn wrap(
        session_id: String,
        task_id: Option<String>,
        sequence_number: u64,
        payload: ServerMessage,
    ) -> Self {
        let message_type = payload.type_tag();
        TaskEnvelope::new(session_id, task_id, message_type, sequence_number, payload)
    }
}

/// Why an inbound [`TaskEnvelope`] failed validation in
/// [`InboundEnvelopeState::validate_inbound`]. Distinct from a JSON parse
/// failure (see `mac-daemon`'s `ProtocolHandler::accept`'s malformed-
/// envelope-vs-malformed-payload handling) -- this is for envelopes that
/// parsed fine but fail the envelope-level contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvelopeRejection {
    /// `now_unix_ms() > envelope.expires_at`.
    Expired { expires_at: u64, now: u64 },
    /// `message_id` was already seen on this connection.
    DuplicateMessageId { message_id: String },
    /// `sequence_number` did not strictly increase over the last one seen
    /// on this connection (including the degenerate first-message case
    /// being fine -- only a repeat/regression is rejected).
    SequenceNotMonotonic { got: u64, last_seen: u64 },
}

impl std::fmt::Display for EnvelopeRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnvelopeRejection::Expired { expires_at, now } => {
                write!(f, "envelope expired: expires_at={expires_at} now={now}")
            }
            EnvelopeRejection::DuplicateMessageId { message_id } => {
                write!(f, "duplicate message_id: {message_id}")
            }
            EnvelopeRejection::SequenceNotMonotonic { got, last_seen } => {
                write!(
                    f,
                    "sequence_number did not increase: got={got} last_seen={last_seen}"
                )
            }
        }
    }
}

/// Per-connection *inbound* envelope-validation state: the in-memory
/// `message_id` seen-set (duplicate rejection) plus the last accepted
/// inbound `sequence_number` (monotonicity rejection).
///
/// Owned entirely by `mac-daemon`'s `ProtocolHandler::accept` read loop
/// (never shared with the writer task -- see `OutboundEnvelopeState` in
/// `mac-daemon/src/control_channel.rs` for that side). Lives for exactly
/// one accepted connection's lifetime and is dropped with it: nothing here
/// is persisted across connections/restarts (a reconnecting client's
/// sequence numbering starts over, same as the seen-set starting empty
/// again), matching this crate's existing one-connection-at-a-time model
/// (see that `ProtocolHandler::accept`'s own doc on `events_tx`/
/// `HoloBridge` sharing).
pub struct InboundEnvelopeState {
    seen_message_ids: HashSet<String>,
    last_inbound_sequence: Option<u64>,
}

impl InboundEnvelopeState {
    pub fn new() -> Self {
        InboundEnvelopeState {
            seen_message_ids: HashSet::new(),
            last_inbound_sequence: None,
        }
    }

    /// Validates an inbound envelope against this connection's state:
    /// expiry, duplicate `message_id`, and `sequence_number`
    /// monotonicity, in that order (expiry first since it needs no mutable
    /// state and is the cheapest check; duplicate-id before sequence since
    /// a replayed exact-duplicate message is a more specific diagnosis
    /// than "sequence didn't move forward").
    ///
    /// On `Ok(())`, this call has side effects: `message_id` is recorded
    /// into the seen-set and `sequence_number` becomes the new
    /// `last_inbound_sequence` -- so this must only be called once per
    /// envelope actually being accepted, never speculatively.
    pub fn validate_inbound<T>(
        &mut self,
        envelope: &TaskEnvelope<T>,
    ) -> std::result::Result<(), EnvelopeRejection> {
        let now = now_unix_ms();
        if envelope.is_expired_at(now) {
            return Err(EnvelopeRejection::Expired {
                expires_at: envelope.expires_at,
                now,
            });
        }

        if self.seen_message_ids.contains(&envelope.message_id) {
            return Err(EnvelopeRejection::DuplicateMessageId {
                message_id: envelope.message_id.clone(),
            });
        }

        if let Some(last) = self.last_inbound_sequence {
            if envelope.sequence_number <= last {
                return Err(EnvelopeRejection::SequenceNotMonotonic {
                    got: envelope.sequence_number,
                    last_seen: last,
                });
            }
        }

        self.seen_message_ids.insert(envelope.message_id.clone());
        self.last_inbound_sequence = Some(envelope.sequence_number);
        Ok(())
    }
}

impl Default for InboundEnvelopeState {
    fn default() -> Self {
        Self::new()
    }
}

/// A message sent from the iOS app to the Mac daemon over the control
/// channel.
///
/// Wire schema: see `holoiroh/PROTOCOL.md` ("ClientMessage").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// A typed text instruction for the `holo-desktop-cli` bridge.
    Prompt {
        text: String,
    },
    /// A voice instruction, already transcribed to text client-side.
    VoiceTranscript {
        text: String,
    },
    /// Cancel/interrupt whatever is currently running.
    Stop,
    /// Presents a PIN for first-connection auth (see `holoiroh/PAIRING.md`'s
    /// "Auth beyond ticket possession" section and `ControlChannel::accept`'s
    /// gate). Added additive-only per `PROTOCOL.md`'s extension policy: an
    /// older client that never sends this variant is simply never let past
    /// the gate for an unrecognized device -- existing `prompt`/
    /// `voice_transcript`/`stop` semantics are unchanged for already-
    /// allowlisted devices, which don't need to send `Pin` at all.
    Pin {
        pin: String,
    },
    /// The user's answer to a [`ServerMessage::InputRequest`]. Carries a
    /// **structured choice selection only** -- the `selected_option` field
    /// is expected to be one of the strings the corresponding
    /// `InputRequest::response_options` offered (or, for kinds like
    /// `sensitive_access_consent` that model a yes/no gate, one of the
    /// options the daemon listed for that purpose). This variant can never
    /// carry a credential, password, MFA code, or other raw manual input --
    /// per the Project Aro PRD's P0-14 requirement, real credential entry is
    /// designed to flow through a **separate `manual_input` channel** that
    /// never reaches the model/agent context (and is not implemented by
    /// this wire schema at all; see [`ServerMessage::InputRequest`]'s doc
    /// for the full rationale). A client must never put a password/PIN/MFA
    /// code string into `selected_option` -- there is intentionally no field
    /// shape here that would make that natural (no free-text field), only a
    /// selection among the pre-enumerated `response_options`.
    InputResponse {
        /// Echoes the `request_id` of the [`ServerMessage::InputRequest`]
        /// this is answering, so the daemon can match it against the
        /// pending request it is tracking (and reject/ignore a response to
        /// an already-expired or unknown request -- see
        /// `ControlChannel::accept`'s pending-input-request handling).
        request_id: String,
        /// The chosen option, expected to be a member of the original
        /// request's `response_options`. Structured selection only -- never
        /// a free-text or credential value (see variant doc above).
        selected_option: String,
    },
}

/// Classifies *why* the daemon is asking the user for input via
/// [`ServerMessage::InputRequest`]. Matches the five kinds named in the
/// Project Aro PRD's P0-14 requirement verbatim; see that variant's doc for
/// the full contract this classification serves.
///
/// Serializes as a bare snake_case string (`#[serde(rename_all =
/// "snake_case")]` with no associated data), so it sits directly in the
/// `kind` field of the wire JSON, e.g. `"kind":"mfa"` -- not a nested tagged
/// object, since none of these five kinds carry kind-specific fields beyond
/// what `InputRequest`'s own `context`/`response_options` already provide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputRequestKind {
    /// A credential (password, API key, secret, etc.) is needed to proceed.
    /// **This kind never carries the credential itself** -- only the fact
    /// that one is needed and why (via `context`). The actual credential
    /// value is out of scope for this wire message entirely; see
    /// [`ServerMessage::InputRequest`]'s doc for where it's designed to go
    /// instead (a separate `manual_input` channel, not part of this schema).
    Credential,
    /// A multi-factor authentication code/approval is needed.
    Mfa,
    /// The agent found more than one plausible way to proceed and needs the
    /// user to pick one -- `response_options` carries the candidates.
    AmbiguousChoice,
    /// The agent is missing some piece of information it cannot infer or
    /// safely guess (e.g. "which calendar account should I use?").
    MissingInfo,
    /// The next step would touch something sensitive (financial action,
    /// destructive operation, access to private data) and needs explicit
    /// user consent before the agent proceeds.
    SensitiveAccessConsent,
}

impl ClientMessage {
    /// The wire `type` discriminant for this variant, as a `&'static str`
    /// matching exactly what `#[serde(tag = "type", rename_all =
    /// "snake_case")]` would serialize (`"prompt"`, `"voice_transcript"`,
    /// `"stop"`, `"pin"`). Used by [`TaskEnvelope::<ClientMessage>::wrap`]
    /// to stamp the envelope-level `message_type` field without
    /// re-parsing the payload's own serialized JSON. Same
    /// not-called-from-the-bin-target status as `wrap` itself (see that
    /// method's doc) -- kept `pub` rather than private since a real dial-
    /// side client implementation needs it too.
    #[allow(dead_code)]
    pub fn type_tag(&self) -> &'static str {
        match self {
            ClientMessage::Prompt { .. } => "prompt",
            ClientMessage::VoiceTranscript { .. } => "voice_transcript",
            ClientMessage::Stop => "stop",
            ClientMessage::Pin { .. } => "pin",
            ClientMessage::InputResponse { .. } => "input_response",
        }
    }
}

/// A message sent from the Mac daemon to the iOS app over the control
/// channel.
///
/// Wire schema: see `holoiroh/PROTOCOL.md` ("ServerMessage").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Acknowledges receipt of a [`ClientMessage`].
    Ack {
        #[serde(skip_serializing_if = "Option::is_none", default)]
        text: Option<String>,
    },
    /// A general daemon/connection status update.
    Status {
        #[serde(skip_serializing_if = "Option::is_none", default)]
        text: Option<String>,
    },
    /// Something failed; `text` should be human-readable detail.
    Error {
        #[serde(skip_serializing_if = "Option::is_none", default)]
        text: Option<String>,
    },
    /// An in-progress update from the `holo-desktop-cli` bridge.
    TaskProgress {
        #[serde(skip_serializing_if = "Option::is_none", default)]
        text: Option<String>,
    },
    /// Sent immediately before the daemon closes the connection because the
    /// peer failed the auth gate in `ControlChannel::accept` (unknown
    /// device id and no/wrong PIN, or auth is required but not yet
    /// configured for this session). Distinct from [`ServerMessage::Error`]
    /// so a client can special-case "show the pairing/PIN screen again"
    /// versus "show a generic error toast".
    AuthRejected {
        #[serde(skip_serializing_if = "Option::is_none", default)]
        text: Option<String>,
    },
    /// Asks the user for structured input the agent cannot proceed without
    /// (Project Aro PRD, row P0-14). This variant is **metadata only**: it
    /// describes *what* is needed and *why*, plus (when applicable) a closed
    /// set of choices the user can pick from -- it can never carry the
    /// actual sensitive value itself (a password, an MFA code, an API key).
    ///
    /// This is a hard invariant, not just a convention: every field on this
    /// variant is `String`/`Vec<String>`/`u64` metadata describing the
    /// *request*, and this type has no constructor path that accepts or
    /// forwards a credential value into any of them. Real credential/manual
    /// input is designed to flow over a **separate `manual_input` channel**
    /// (out of this wire schema's scope -- not implemented here) that is
    /// architected to never reach the model/agent context at all, matching
    /// the PRD's explicit requirement that "credential characters are never
    /// logged, never included in screenshots, never echoed in task events".
    /// Concretely: nothing at any layer of this daemon should ever call
    /// `ServerMessage::input_request` (or construct this variant directly)
    /// with a raw secret as an argument -- `context`/`response_options`
    /// exist to describe the *shape* of what's needed ("GitHub personal
    /// access token", "the 6-digit code from your authenticator app"), never
    /// to hold the value.
    ///
    /// The user's answer comes back as [`ClientMessage::InputResponse`] for
    /// `AmbiguousChoice`/`MissingInfo`/`SensitiveAccessConsent` kinds (a
    /// selection among `response_options`); for `Credential`/`Mfa` kinds,
    /// this message only ever announces that manual entry is needed --
    /// providing the actual secret is out of band for this channel entirely
    /// (the separate `manual_input` channel above).
    ///
    /// See `ControlChannel::accept`'s pending-input-request tracking for
    /// how `expires_at` is enforced (expiry-to-safe-pause: no
    /// `InputResponse` before `expires_at` emits a
    /// [`ServerMessage::Status`] saying the task safely paused, not an
    /// error).
    InputRequest {
        /// Correlates this request with the eventual
        /// [`ClientMessage::InputResponse`] (or with the safe-pause
        /// [`ServerMessage::Status`] emitted on expiry).
        request_id: String,
        /// Which of the five PRD-defined kinds this request is.
        kind: InputRequestKind,
        /// Human-readable explanation of what's needed and why (e.g.
        /// "Holo needs your GitHub personal access token to push this
        /// branch" or "Two calendars match 'team standup' -- which one?").
        /// Never contains a credential value itself (see variant doc above).
        context: String,
        /// The closed set of choices the user may pick from, echoed back
        /// verbatim as [`ClientMessage::InputResponse::selected_option`].
        /// May legitimately be empty for kinds that don't have discrete
        /// choices to offer (e.g. `Credential`/`Mfa`, which are simply
        /// announcing that out-of-band manual entry is needed with no
        /// enumerable options) -- an empty `Vec` serializes as `[]`, not
        /// omitted, so the client can always rely on the field being
        /// present.
        response_options: Vec<String>,
        /// Unix epoch milliseconds after which this request is considered
        /// expired if no [`ClientMessage::InputResponse`] has arrived. Plain
        /// `u64` epoch millis (not `chrono`/`time`, neither of which this
        /// crate depends on) -- JSON has no native timestamp type, so this
        /// is the simplest unambiguous wire representation; see
        /// [`Self::input_request`] for how it's computed from
        /// `std::time::SystemTime`.
        expires_at: u64,
    },
}

impl ServerMessage {
    /// The wire `type` discriminant for this variant (see
    /// [`ClientMessage::type_tag`] for the identical rationale). Used by
    /// [`TaskEnvelope::<ServerMessage>::wrap`].
    pub fn type_tag(&self) -> &'static str {
        match self {
            ServerMessage::Ack { .. } => "ack",
            ServerMessage::Status { .. } => "status",
            ServerMessage::Error { .. } => "error",
            ServerMessage::TaskProgress { .. } => "task_progress",
            ServerMessage::AuthRejected { .. } => "auth_rejected",
            ServerMessage::InputRequest { .. } => "input_request",
        }
    }

    /// Convenience constructor for a bare `{"type":"ack"}` with no text.
    pub fn ack() -> Self {
        ServerMessage::Ack { text: None }
    }

    /// Convenience constructor for a `status` message with text.
    pub fn status(text: impl Into<String>) -> Self {
        ServerMessage::Status {
            text: Some(text.into()),
        }
    }

    /// Convenience constructor for an `error` message with text.
    pub fn error(text: impl Into<String>) -> Self {
        ServerMessage::Error {
            text: Some(text.into()),
        }
    }

    /// Convenience constructor for a `task_progress` message with text.
    pub fn task_progress(text: impl Into<String>) -> Self {
        ServerMessage::TaskProgress {
            text: Some(text.into()),
        }
    }

    /// Convenience constructor for an `auth_rejected` message with text.
    pub fn auth_rejected(text: impl Into<String>) -> Self {
        ServerMessage::AuthRejected {
            text: Some(text.into()),
        }
    }

    /// Convenience constructor for an `input_request` message. `ttl` is how
    /// long from *now* (real wall-clock time, via [`epoch_millis_now`]) the
    /// request stays valid; `expires_at` is computed here so every call site
    /// works in relative durations rather than hand-computing epoch millis.
    ///
    /// Deliberately takes only metadata-shaped arguments
    /// (`kind`/`context`/`response_options`) -- there is no parameter here
    /// through which a credential value could be threaded, by construction
    /// (see [`ServerMessage::InputRequest`]'s doc for why that matters).
    ///
    /// Not yet called from `main.rs`'s binary path -- this row wires the
    /// wire-protocol type, the `PendingInputRequest` tracking, and the
    /// expiry-to-safe-pause mechanics in `ControlChannel::accept`; the
    /// *trigger* (some real signal from `holo_bridge`/`holo-desktop-cli`
    /// indicating a turn actually needs credential/MFA/choice/consent
    /// input) does not exist in `holo_bridge::control::ControlEvent` today
    /// and is explicitly out of scope here rather than fabricated (see this
    /// task's PRD notes) -- same "real API, not yet wired into `main.rs`"
    /// status as `ControlChannel::send_on_new_stream` and `read_line`
    /// above. Exercised directly (not via the daemon binary) by
    /// `mac-daemon/examples/input_request_probe.rs`.
    #[allow(dead_code)]
    pub fn input_request(
        request_id: impl Into<String>,
        kind: InputRequestKind,
        context: impl Into<String>,
        response_options: Vec<String>,
        ttl: std::time::Duration,
    ) -> Self {
        ServerMessage::InputRequest {
            request_id: request_id.into(),
            kind,
            context: context.into(),
            response_options,
            expires_at: epoch_millis_now().saturating_add(ttl.as_millis() as u64),
        }
    }

    /// Convenience constructor for the `status` message emitted when a
    /// pending [`ServerMessage::InputRequest`] expires with no
    /// [`ClientMessage::InputResponse`] -- see `ControlChannel::accept`'s
    /// pending-input-request expiry handling. Deliberately a `Status`,
    /// never an `Error`: expiry is a safe, expected outcome (the task
    /// pauses cleanly, waiting for the user), not a failure -- matching the
    /// task's explicit "safely paused (not failed)" requirement.
    ///
    /// `mac-daemon`'s own `ControlChannel::accept` expiry arm routes
    /// through `ControlEvent::DaemonStatus` instead of calling this
    /// directly (its `send` half of the stream is owned by the writer task
    /// by the time expiry can fire, so it cannot construct a
    /// `ServerMessage` and write it inline) -- both paths share
    /// [`input_request_expired_text`] so the wording stays identical. Kept
    /// as public API for any future direct caller (e.g. a test/probe
    /// building the message without going through the full connection
    /// loop) and exercised directly by
    /// `mac-daemon/examples/input_request_probe.rs`.
    #[allow(dead_code)]
    pub fn input_request_expired(request_id: impl Into<String>) -> Self {
        ServerMessage::Status {
            text: Some(input_request_expired_text(&request_id.into())),
        }
    }
}

/// Current wall-clock time as Unix epoch milliseconds, saturating to `0` in
/// the (practically impossible on any real system clock) case that
/// [`std::time::SystemTime::now`] reports a time before
/// [`std::time::UNIX_EPOCH`]. Used to compute
/// [`ServerMessage::InputRequest::expires_at`]
/// ([`ServerMessage::input_request`]) and to check pending-request expiry
/// in `mac-daemon`'s `ControlChannel::accept`.
pub fn epoch_millis_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The human-readable text for the safe-pause status emitted when an
/// [`ServerMessage::InputRequest`] expires with no
/// [`ClientMessage::InputResponse`]. Shared by
/// [`ServerMessage::input_request_expired`] (the direct constructor) and
/// `mac-daemon`'s `ControlChannel::accept` expiry arm (which routes through
/// `ControlEvent::DaemonStatus` instead, since `send` is owned by the
/// writer task by the time expiry can fire) so both paths emit identical
/// wording rather than two independently-maintained copies.
pub fn input_request_expired_text(request_id: &str) -> String {
    format!(
        "input request {request_id} expired with no response -- task safely paused, waiting for input"
    )
}

/// Serializes `msg` as one newline-delimited JSON line and writes it to
/// `send`, flushing afterwards. Used by both the accept side (writing
/// `ServerMessage`) and the dial side (writing `ClientMessage`) -- generic
/// over any `Serialize` payload so both directions share one write path.
pub async fn write_line<T, W>(send: &mut W, msg: &T) -> Result<(), std::io::Error>
where
    T: Serialize,
    W: tokio::io::AsyncWrite + Unpin,
{
    // `serde_json::to_string` can only fail on a type with a broken
    // `Serialize` impl (e.g. a map with non-string keys) -- none of this
    // crate's wire types can produce that, so a failure here is mapped to
    // `std::io::Error` (via `io::Error::other`) rather than pulling in an
    // `anyhow`/`thiserror` dependency just for this one, effectively-never
    // path. Every real failure mode below (`write_all`/`flush`) is already
    // `std::io::Error`.
    let mut line = serde_json::to_string(msg)
        .map_err(|err| std::io::Error::other(format!("serializing control message: {err}")))?;
    line.push('\n');
    send.write_all(line.as_bytes()).await?;
    send.flush().await?;
    Ok(())
}

/// Reads one newline-delimited JSON line from `lines` and deserializes it
/// as `T`.
///
/// Returns `Ok(None)` on clean EOF (peer closed the stream) and on blank
/// lines (tolerated as harmless keep-alive-ish input, not a parse error).
/// Returns `Ok(Some(Err(..)))` when a non-blank line was read but failed to
/// parse -- this is deliberately *not* folded into the outer `Err` case,
/// matching `PROTOCOL.md`'s "Error handling on malformed input": a bad line
/// is not a transport-level failure, the stream stays open and the caller
/// decides how to respond (e.g. send back a `ServerMessage::error` and keep
/// reading). The outer `Err` is reserved for actual I/O failures on the
/// stream itself.
// Not yet called from the daemon binary (the accept-side loop in
// `mac-daemon`'s `ProtocolHandler::accept` reads lines inline rather than
// through this helper), but it's the natural read-side counterpart to
// `write_line` and the one a future dial-side implementation (this daemon
// acting as a client of *another* holoiroh-daemon, or a Rust test harness
// standing in for the iOS app) will need -- kept as public API rather than
// deleted and reintroduced later.
#[allow(dead_code)]
pub async fn read_line<T, R>(
    lines: &mut tokio::io::Lines<R>,
) -> Result<Option<std::result::Result<T, serde_json::Error>>, std::io::Error>
where
    T: for<'de> Deserialize<'de>,
    R: tokio::io::AsyncBufRead + Unpin,
{
    loop {
        match lines.next_line().await? {
            None => return Ok(None),
            Some(line) if line.trim().is_empty() => continue,
            Some(line) => return Ok(Some(serde_json::from_str::<T>(&line))),
        }
    }
}
