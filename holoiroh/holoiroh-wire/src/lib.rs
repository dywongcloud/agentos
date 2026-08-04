//! Defines platform-neutral wire types for the control channel.
//!
//! The daemon and bridge share these serde types, validation functions, and signing formats.
//! The crate cross-compiles to `aarch64-apple-ios` and `wasm32-wasip1`.
//! It does not use `iroh`, an asynchronous runtime, or macOS-specific application programming interfaces (APIs).
//!
//! Connection handling remains in `mac-daemon/src/control_channel.rs`.
//! This handling includes `ProtocolHandler`, authentication, router wiring, `OutboundEnvelopeState`, and audit-log state.
//! `ServerMessage::from_control_event` also remains there because it translates the internal `holo_bridge::ControlEvent` and `DoneStatus` types.
//!
//! See `holoiroh/PROTOCOL.md` for the authoritative wire schema.
//! Keep that document and this file synchronized.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

/// Identifies the control channel on the shared `iroh` `Endpoint`.
/// This application-layer protocol negotiation (ALPN) value follows the `iroh_moq::ALPN` and `iroh_gossip::ALPN` convention.
pub const CONTROL_ALPN: &[u8] = b"holoiroh/control/1";

/// Sets the Project Aro product requirements document (PRD) task-envelope schema version.
/// Change this value only for a coordinated wire-format change.
/// See the "Envelope versioning" section in `PROTOCOL.md`.
/// This value is independent of the crate version in `Cargo.toml`.
pub const PROTOCOL_VERSION: u32 = 1;

/// Sets the default envelope lifetime to 30,000 milliseconds.
/// [`TaskEnvelope::new`] calculates `expires_at` as `sent_at + DEFAULT_EXPIRY_MS`.
pub const DEFAULT_EXPIRY_MS: u64 = 30_000;

/// Binds a signature to one control-channel direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EnvelopeDirection {
    ClientToDaemon = 1,
    DaemonToClient = 2,
}

impl EnvelopeDirection {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

/// A payload could not be converted to canonical signing bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SigningPayloadError {
    PayloadSerialization { message: String },
    LengthOverflow,
}

impl std::fmt::Display for SigningPayloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SigningPayloadError::PayloadSerialization { message } => {
                write!(f, "payload serialization failed: {message}")
            }
            SigningPayloadError::LengthOverflow => {
                f.write_str("canonical field length exceeds u64")
            }
        }
    }
}

impl std::error::Error for SigningPayloadError {}

/// A signature string did not use the strict Ed25519 wire format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureCodecError {
    WrongPrefix,
    WrongLength { got: usize },
    UppercaseHex { index: usize },
    NonHex { index: usize, byte: u8 },
}

impl std::fmt::Display for SignatureCodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SignatureCodecError::WrongPrefix => f.write_str("signature must start with ed25519:"),
            SignatureCodecError::WrongLength { got } => {
                write!(f, "signature hex must contain 128 characters; got {got}")
            }
            SignatureCodecError::UppercaseHex { index } => {
                write!(f, "signature hex contains uppercase at byte {index}")
            }
            SignatureCodecError::NonHex { index, byte } => {
                write!(
                    f,
                    "signature hex contains non-hex byte 0x{byte:02x} at byte {index}"
                )
            }
        }
    }
}

impl std::error::Error for SignatureCodecError {}

const ED25519_SIGNATURE_PREFIX: &str = "ed25519:";
const ED25519_SIGNATURE_HEX_BYTES: usize = 128;

/// Encodes 64 signature bytes as `ed25519:` plus 128 lowercase hex bytes.
pub fn encode_ed25519_signature(signature: &[u8; 64]) -> String {
    const LOWER_HEX: &[u8; 16] = b"0123456789abcdef";

    let mut encoded =
        String::with_capacity(ED25519_SIGNATURE_PREFIX.len() + ED25519_SIGNATURE_HEX_BYTES);
    encoded.push_str(ED25519_SIGNATURE_PREFIX);
    for byte in signature {
        encoded.push(LOWER_HEX[(byte >> 4) as usize] as char);
        encoded.push(LOWER_HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

/// Decodes the exact lowercase Ed25519 signature wire format.
pub fn decode_ed25519_signature(
    encoded: &str,
) -> std::result::Result<[u8; 64], SignatureCodecError> {
    let Some(hex) = encoded.strip_prefix(ED25519_SIGNATURE_PREFIX) else {
        return Err(SignatureCodecError::WrongPrefix);
    };
    if hex.len() != ED25519_SIGNATURE_HEX_BYTES {
        return Err(SignatureCodecError::WrongLength { got: hex.len() });
    }

    let mut signature = [0_u8; 64];
    for (index, pair) in hex.as_bytes().chunks_exact(2).enumerate() {
        let high = decode_lower_hex(pair[0], index * 2)?;
        let low = decode_lower_hex(pair[1], index * 2 + 1)?;
        signature[index] = (high << 4) | low;
    }
    Ok(signature)
}

fn decode_lower_hex(byte: u8, index: usize) -> std::result::Result<u8, SignatureCodecError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Err(SignatureCodecError::UppercaseHex { index }),
        _ => Err(SignatureCodecError::NonHex { index, byte }),
    }
}

/// Returns the current wall-clock time in Unix epoch milliseconds.
///
/// The `u64` type follows the timestamp convention in `allowlist.rs`.
/// `AllowlistEntry::paired_at` uses `u64` seconds.
/// Millisecond precision supports the 30-second default expiry window and subsecond values such as 30,500 milliseconds.
fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Wraps a [`ClientMessage`] or [`ServerMessage`] in the Project Aro PRD task-envelope schema.
/// `TaskEnvelope<ClientMessage>` travels from the app to the daemon.
/// `TaskEnvelope<ServerMessage>` travels from the daemon to the app.
/// See the "Envelope" section in `PROTOCOL.md` for field descriptions and wire examples.
///
/// ## Field notes
///
/// - `message_type` repeats the payload's internal `type` discriminant.
///   [`TaskEnvelope::new`] derives it from the payload.
///   This redundancy makes framing fields inspectable before typed payload deserialization.
///   It also matches the PRD's flat envelope shape.
/// - `session_id` identifies one accepted `iroh` connection.
///   `OutboundEnvelopeState::new` in `mac-daemon/src/control_channel.rs` creates it.
///   It remains stable for the connection lifetime.
///   Every envelope carries it in both directions.
///   [`InboundEnvelopeState::validate_inbound`] validates inbound values.
/// - `task_id` correlates the envelope with a `ControlMessage` or `ControlEvent` turn.
///   See the `to_control_message` and `from_control_event` calls in `ProtocolHandler::accept`.
///   `None` permits messages without bridge-turn correlation.
///   Examples include the initial greeting and a `stop` without a target task.
/// - `sent_at` and `expires_at` use Unix epoch milliseconds.
///   [`InboundEnvelopeState::validate_inbound`] rejects an envelope only when `now_unix_ms() > expires_at`.
///   Equality remains valid.
/// - `signature` starts as `None` in constructors.
///   Transport adapters sign every post-authentication envelope with the sending endpoint's Ed25519 key.
///   They verify the signature against the authenticated peer before parsing the typed payload.
///   They also verify it before changing inbound state.
///   This crate supplies the canonical signing bytes and strict wire codec.
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
    /// Creates an envelope for `payload`.
    /// The function sets `sent_at` to the current time.
    /// It sets `expires_at` to `sent_at + DEFAULT_EXPIRY_MS`.
    /// It creates a new `message_id` and sets `signature` to `None`.
    ///
    /// The caller must supply the connection-scoped `sequence_number`.
    /// See `OutboundEnvelopeState::next_outbound_sequence` in `mac-daemon/src/control_channel.rs`.
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

    /// Reports whether `now` is later than `expires_at`.
    /// Equality remains valid.
    /// [`InboundEnvelopeState::validate_inbound`] uses the same check.
    /// `mac-daemon/examples/envelope_probe.rs` provides a standalone witness.
    pub fn is_expired_at(&self, now: u64) -> bool {
        now > self.expires_at
    }
}

const SIGNING_TAG_DOMAIN: u8 = 1;
const SIGNING_TAG_ENVELOPE_KIND: u8 = 2;
const SIGNING_TAG_ALGORITHM: u8 = 3;
const SIGNING_TAG_SIGNATURE_VERSION: u8 = 4;
const SIGNING_TAG_DIRECTION: u8 = 5;
const SIGNING_TAG_SIGNER: u8 = 6;
const SIGNING_TAG_RECIPIENT: u8 = 7;
const SIGNING_TAG_PROTOCOL_VERSION: u8 = 8;
const SIGNING_TAG_MESSAGE_ID: u8 = 9;
const SIGNING_TAG_SESSION_ID: u8 = 10;
const SIGNING_TAG_TASK_ID: u8 = 11;
const SIGNING_TAG_MESSAGE_TYPE: u8 = 12;
const SIGNING_TAG_SENT_AT: u8 = 13;
const SIGNING_TAG_EXPIRES_AT: u8 = 14;
const SIGNING_TAG_SEQUENCE_NUMBER: u8 = 15;
const SIGNING_TAG_PAYLOAD: u8 = 16;

const CANONICAL_JSON_NULL: u8 = 1;
const CANONICAL_JSON_BOOL: u8 = 2;
const CANONICAL_JSON_NUMBER: u8 = 3;
const CANONICAL_JSON_STRING: u8 = 4;
const CANONICAL_JSON_ARRAY: u8 = 5;
const CANONICAL_JSON_OBJECT: u8 = 6;

impl<T: Serialize> TaskEnvelope<T> {
    /// Creates canonical bytes for an Ed25519 signature without serializing the envelope.
    ///
    /// Each envelope field starts with a one-byte tag and an eight-byte big-endian length.
    /// Integers use fixed-width big-endian bytes.
    /// The output excludes the signature field.
    /// JavaScript Object Notation (JSON) values use explicit type tags.
    /// Object keys use UTF-8 lexical order at every nesting level.
    ///
    /// The method materializes `payload` as one `serde_json::Value`.
    /// It writes canonical payload bytes directly into the returned buffer.
    /// It does not allocate a second canonical payload buffer.
    /// A large owned string can exist simultaneously in the input, JSON value, and result.
    /// Callers must enforce the input limit before this call.
    /// Serialization or length overflow returns [`SigningPayloadError`].
    pub fn signing_payload(
        &self,
        direction: EnvelopeDirection,
        signer: &[u8; 32],
        recipient: &[u8; 32],
    ) -> std::result::Result<Vec<u8>, SigningPayloadError> {
        let payload = serde_json::to_value(&self.payload).map_err(|error| {
            SigningPayloadError::PayloadSerialization {
                message: error.to_string(),
            }
        })?;
        let mut output = Vec::new();

        append_signing_field(&mut output, SIGNING_TAG_DOMAIN, CONTROL_ALPN)?;
        append_signing_field(&mut output, SIGNING_TAG_ENVELOPE_KIND, b"task-envelope")?;
        append_signing_field(&mut output, SIGNING_TAG_ALGORITHM, b"ed25519")?;
        append_signing_field(&mut output, SIGNING_TAG_SIGNATURE_VERSION, b"signature-v1")?;
        append_signing_field(&mut output, SIGNING_TAG_DIRECTION, &[direction.as_u8()])?;
        append_signing_field(&mut output, SIGNING_TAG_SIGNER, signer)?;
        append_signing_field(&mut output, SIGNING_TAG_RECIPIENT, recipient)?;
        append_signing_field(
            &mut output,
            SIGNING_TAG_PROTOCOL_VERSION,
            &self.protocol_version.to_be_bytes(),
        )?;
        append_signing_field(
            &mut output,
            SIGNING_TAG_MESSAGE_ID,
            self.message_id.as_bytes(),
        )?;
        append_signing_field(
            &mut output,
            SIGNING_TAG_SESSION_ID,
            self.session_id.as_bytes(),
        )?;
        append_optional_string_field(&mut output, SIGNING_TAG_TASK_ID, self.task_id.as_deref())?;
        append_signing_field(
            &mut output,
            SIGNING_TAG_MESSAGE_TYPE,
            self.message_type.as_bytes(),
        )?;
        append_signing_field(
            &mut output,
            SIGNING_TAG_SENT_AT,
            &self.sent_at.to_be_bytes(),
        )?;
        append_signing_field(
            &mut output,
            SIGNING_TAG_EXPIRES_AT,
            &self.expires_at.to_be_bytes(),
        )?;
        append_signing_field(
            &mut output,
            SIGNING_TAG_SEQUENCE_NUMBER,
            &self.sequence_number.to_be_bytes(),
        )?;

        output.push(SIGNING_TAG_PAYLOAD);
        let length_offset = output.len();
        output.extend_from_slice(&0_u64.to_be_bytes());
        let payload_offset = output.len();
        append_canonical_json(&mut output, &payload)?;
        let payload_length = u64::try_from(output.len() - payload_offset)
            .map_err(|_| SigningPayloadError::LengthOverflow)?;
        output[length_offset..payload_offset].copy_from_slice(&payload_length.to_be_bytes());

        Ok(output)
    }
}

fn append_signing_field(
    output: &mut Vec<u8>,
    tag: u8,
    value: &[u8],
) -> std::result::Result<(), SigningPayloadError> {
    let length = u64::try_from(value.len()).map_err(|_| SigningPayloadError::LengthOverflow)?;
    output.push(tag);
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(value);
    Ok(())
}

fn append_optional_string_field(
    output: &mut Vec<u8>,
    tag: u8,
    value: Option<&str>,
) -> std::result::Result<(), SigningPayloadError> {
    let value_length = value.map_or(0, str::len);
    let length = value_length
        .checked_add(1)
        .and_then(|length| u64::try_from(length).ok())
        .ok_or(SigningPayloadError::LengthOverflow)?;
    output.push(tag);
    output.extend_from_slice(&length.to_be_bytes());
    match value {
        None => output.push(0),
        Some(value) => {
            output.push(1);
            output.extend_from_slice(value.as_bytes());
        }
    }
    Ok(())
}

fn append_canonical_json(
    output: &mut Vec<u8>,
    value: &serde_json::Value,
) -> std::result::Result<(), SigningPayloadError> {
    match value {
        serde_json::Value::Null => output.push(CANONICAL_JSON_NULL),
        serde_json::Value::Bool(value) => {
            output.push(CANONICAL_JSON_BOOL);
            output.push(u8::from(*value));
        }
        serde_json::Value::Number(value) => {
            output.push(CANONICAL_JSON_NUMBER);
            append_length_prefixed(output, value.to_string().as_bytes())?;
        }
        serde_json::Value::String(value) => {
            output.push(CANONICAL_JSON_STRING);
            append_length_prefixed(output, value.as_bytes())?;
        }
        serde_json::Value::Array(values) => {
            output.push(CANONICAL_JSON_ARRAY);
            append_count(output, values.len())?;
            for value in values {
                append_canonical_json(output, value)?;
            }
        }
        serde_json::Value::Object(values) => {
            output.push(CANONICAL_JSON_OBJECT);
            append_count(output, values.len())?;
            let mut entries: Vec<_> = values.iter().collect();
            entries.sort_unstable_by(|(left, _), (right, _)| left.as_bytes().cmp(right.as_bytes()));
            for (key, value) in entries {
                output.push(CANONICAL_JSON_STRING);
                append_length_prefixed(output, key.as_bytes())?;
                append_canonical_json(output, value)?;
            }
        }
    }
    Ok(())
}

fn append_count(
    output: &mut Vec<u8>,
    count: usize,
) -> std::result::Result<(), SigningPayloadError> {
    let count = u64::try_from(count).map_err(|_| SigningPayloadError::LengthOverflow)?;
    output.extend_from_slice(&count.to_be_bytes());
    Ok(())
}

fn append_length_prefixed(
    output: &mut Vec<u8>,
    value: &[u8],
) -> std::result::Result<(), SigningPayloadError> {
    append_count(output, value.len())?;
    output.extend_from_slice(value);
    Ok(())
}

impl TaskEnvelope<ClientMessage> {
    /// Wraps a client payload and derives `message_type` from its discriminant.
    /// This derivation keeps the envelope value synchronized with the [`ClientMessage`] variant.
    ///
    /// The daemon receives these envelopes but does not construct them in its binary target.
    /// Dial-side clients and probes use this function.
    /// Witnesses include `mac-daemon/examples/control_probe.rs` and `mac-daemon/examples/control_channel_probe.rs`.
    /// This function has the same probe-support status as `AuthState::for_probing`.
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

/// Describes an envelope-level validation failure from [`InboundEnvelopeState::validate_inbound`].
/// JavaScript Object Notation (JSON) parse failures use a separate path.
/// See malformed-envelope and malformed-payload handling in `ProtocolHandler::accept`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvelopeRejection {
    /// The envelope belongs to a different connection session.
    SessionMismatch { expected: String, got: String },
    /// `now_unix_ms() > envelope.expires_at`.
    Expired { expires_at: u64, now: u64 },
    /// `message_id` was already seen on this connection.
    DuplicateMessageId { message_id: String },
    /// Rejects a repeated or decreased `sequence_number`.
    /// The first sequence number can have any value.
    SequenceNotMonotonic { got: u64, last_seen: u64 },
}

impl std::fmt::Display for EnvelopeRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnvelopeRejection::SessionMismatch { expected, got } => {
                write!(f, "session_id mismatch: expected={expected} got={got}")
            }
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

/// Stores inbound envelope-validation state for one connection.
/// An optional session binding precedes the replay set and last accepted sequence number.
///
/// The `ProtocolHandler::accept` read loop owns this state exclusively.
/// It does not share the state with the writer task.
/// See `OutboundEnvelopeState` in `mac-daemon/src/control_channel.rs` for writer state.
/// The state exists for exactly one accepted connection and is not persisted.
/// A reconnect starts with an empty replay set and new sequence numbering.
/// This behavior matches the one-connection-at-a-time model.
/// See the `ProtocolHandler::accept` documentation for `events_tx` and `HoloBridge` sharing.
pub struct InboundEnvelopeState {
    expected_session: Option<String>,
    seen_message_ids: HashSet<String>,
    last_inbound_sequence: Option<u64>,
}

impl InboundEnvelopeState {
    /// Creates standalone state without a session binding.
    pub fn new() -> Self {
        InboundEnvelopeState {
            expected_session: None,
            seen_message_ids: HashSet::new(),
            last_inbound_sequence: None,
        }
    }

    /// Creates connection state bound to one expected session identifier.
    pub fn for_session(expected: impl Into<String>) -> Self {
        InboundEnvelopeState {
            expected_session: Some(expected.into()),
            seen_message_ids: HashSet::new(),
            last_inbound_sequence: None,
        }
    }

    /// Validates the session, expiry, duplicate `message_id`, and sequence order.
    /// All rejection checks finish before the function changes replay or sequence state.
    /// Therefore, a session mismatch does not consume either value.
    ///
    /// On success, the function records `message_id` in the replay set.
    /// It also sets `last_inbound_sequence` to `sequence_number`.
    /// Call this function exactly once for each accepted envelope.
    /// Do not call it speculatively.
    pub fn validate_inbound<T>(
        &mut self,
        envelope: &TaskEnvelope<T>,
    ) -> std::result::Result<(), EnvelopeRejection> {
        if let Some(expected) = &self.expected_session
            && envelope.session_id != *expected
        {
            return Err(EnvelopeRejection::SessionMismatch {
                expected: expected.clone(),
                got: envelope.session_id.clone(),
            });
        }

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

        if let Some(last) = self.last_inbound_sequence
            && envelope.sequence_number <= last
        {
            return Err(EnvelopeRejection::SequenceNotMonotonic {
                got: envelope.sequence_number,
                last_seen: last,
            });
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

/// Opaque identity for one proposed action.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ActionId(pub String);

/// Risk classification supplied with an action proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalRisk {
    Low,
    Medium,
    High,
    Critical,
}

/// Structured description of the external effect an action would produce.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalEffect {
    pub app: String,
    pub target: String,
    pub material: String,
}

/// The user's terminal decision for one exact action approval request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approve,
    Deny,
    Cancel,
}

/// Describes one exact action that cannot execute until it is approved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionApprovalRequest {
    pub approval_id: String,
    pub action_id: ActionId,
    pub proposal_digest: String,
    pub run_id: String,
    pub task_id: String,
    pub risk: ApprovalRisk,
    pub effect: ApprovalEffect,
    pub before_state_digest: String,
    pub expires_at: u64,
}

/// Answers one [`ActionApprovalRequest`] without accepting mutable proposal fields from the client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionApprovalResponse {
    pub approval_id: String,
    pub action_id: ActionId,
    pub proposal_digest: String,
    pub decision: ApprovalDecision,
}

/// A signed, authoritative goal for the typed planner path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypedPrompt {
    pub goal_id: String,
    pub instruction: String,
}

/// A planner-produced action proposal. The proposal retains every binding used by the executor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypedActionProposal {
    pub goal_id: String,
    pub intent_digest: String,
    pub run_id: String,
    pub task_id: String,
    pub action_id: ActionId,
    pub observation: TypedObservation,
    pub target: TypedTarget,
    pub action: TypedDesktopAction,
    pub proposal_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypedObservation {
    pub observation_id: String,
    pub before_state_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypedTarget {
    pub bundle_id: String,
    pub window_id: String,
    pub element_id: String,
    pub expected_role: String,
    pub expected_title_digest: String,
    pub expected_value_digest: Option<String>,
    pub sensitive: bool,
    pub credential: bool,
    pub resolved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TypedDesktopAction {
    Observe,
    Navigate { navigation: TypedNavigationAction },
    Focus,
    DraftText { text: String },
    Commit { commit: TypedCommitAction },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TypedNavigationAction {
    SemanticActivate,
    CoordinateActivate { x: i32, y: i32 },
    Scroll { horizontal: i32, vertical: i32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TypedCommitAction {
    SendMessage,
    SubmitForm,
    Publish,
    Purchase,
    TransferFunds,
    DeleteItem,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub plan_id: String,
    pub goal_digest: String,
    pub steps: Vec<PlannedStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlannedStep {
    Action { proposal: TypedActionProposal },
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlannerRunStatus {
    Planning,
    Ready,
    Executing,
    Completed,
    Failed,
    Canceled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannerReceipt {
    pub plan_id: String,
    pub goal_digest: String,
    pub action_id: ActionId,
    pub proposal_digest: String,
    pub status: PlannerRunStatus,
}

/// Represents a control-channel message from the app to the daemon.
///
/// See "ClientMessage" in `holoiroh/PROTOCOL.md` for the wire schema.
// No `Eq`: `RemoteControl` carries `RemoteControlEvent`, which has `f64`
// normalized coordinates (`f64: !Eq`). `PartialEq` is retained.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Sends an authoritative goal to the typed planner and executor path.
    TypedPrompt { prompt: TypedPrompt },
    /// A typed text instruction for the `holo-desktop-cli` bridge.
    Prompt { text: String },
    /// A voice instruction, already transcribed to text client-side.
    VoiceTranscript { text: String },
    /// Stops work.
    /// Without `context_id`, the daemon cancels the running turn, drains the queue, and runs `holo stop`.
    /// With `context_id`, the daemon cancels only that turn.
    /// A scoped stop does not drain the queue or run the global stop.
    /// See the `stop` section in `PROTOCOL.md`.
    /// `None` serializes as `{"type":"stop"}`, matching the earlier unit variant.
    /// The field is additive under the protocol extension policy.
    Stop {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context_id: Option<String>,
    },
    /// Pauses the active turn.
    /// The Holo backend does not provide a pause remote procedure call (RPC) over agent-to-agent (A2A) communication.
    /// Therefore, the daemon cancels the running turn and stores its instruction text and `contextId`.
    /// A later [`ClientMessage::Resume`] dispatches the instruction again with the same `contextId`.
    /// The backend session history then carries the task forward.
    /// This variant is additive under the `PROTOCOL.md` extension policy.
    Pause,
    /// Resumes the turn stored by [`ClientMessage::Pause`].
    /// If no turn is paused, the daemon sends a polite status reply and changes no task state.
    Resume,
    /// Replaces active and queued work with `text`.
    /// The daemon cancels the active turn and drops the queue.
    /// When available, it reuses the canceled turn's `contextId` to preserve agent task history.
    Redirect { text: String },
    /// Provides a personal identification number (PIN) for first-connection authentication.
    /// See "Auth beyond ticket possession" in `holoiroh/PAIRING.md` and the `ControlChannel::accept` gate.
    /// An older client cannot pass the gate for an unknown device without this message.
    /// Allowlisted devices do not need to send `Pin`.
    /// Existing `prompt`, `voice_transcript`, and `stop` behavior remains unchanged.
    /// This variant is additive under the `PROTOCOL.md` extension policy.
    Pin { pin: String },
    /// Carries the user's structured answer to a [`ServerMessage::InputRequest`].
    /// `selected_option` must equal one offered `InputRequest::response_options` value.
    /// This rule also applies to yes-or-no `sensitive_access_consent` requests.
    /// Callers must not insert credentials, passwords, personal identification numbers (PINs), or multi-factor authentication (MFA) codes.
    /// The string fields do not structurally enforce this prohibition.
    /// Project Aro PRD requirement P0-14 assigns secret entry to a separate `manual_input` channel.
    /// This schema does not implement that channel.
    InputResponse {
        /// Identifies the corresponding [`ServerMessage::InputRequest`].
        /// The daemon rejects or ignores an expired or unknown request.
        /// See the pending-input-request handling in `ControlChannel::accept`.
        request_id: String,
        /// Contains one value from the original `response_options`.
        /// Callers must not use this field for free text or credentials.
        selected_option: String,
    },
    /// Carries a terminal response to one exact pending action approval.
    ApprovalResponse {
        #[serde(flatten)]
        response: ActionApprovalResponse,
    },
    /// Starts hands-on control from the app.
    /// The user drives the Mac by touching the media stream view.
    /// `event` contains one normalized touch-derived action.
    /// See [`RemoteControlEvent`].
    /// This variant is additive under the `PROTOCOL.md` extension policy.
    RemoteControl { event: RemoteControlEvent },
    /// Requests clarifying questions before the daemon runs a possibly ambiguous instruction.
    /// The daemon calls its clarification model and returns [`ServerMessage::ClarifyQuestions`].
    /// An empty response means the instruction is clear.
    /// This message does not dispatch `prompt` to the desktop backend.
    /// After the user answers, the app sends a separate [`ClientMessage::Prompt`].
    /// This variant bypasses the task pipeline and is additive under the `PROTOCOL.md` extension policy.
    ClarifyRequest { prompt: String },
    /// Requests Markdown conversion for an attached document.
    /// The daemon uses the Tinfoil processing in `mac-daemon::tinfoil_documents`.
    /// It returns [`ServerMessage::DocumentProcessed`] or [`ServerMessage::DocumentProcessFailed`].
    /// This variant is additive under the `PROTOCOL.md` extension policy.
    ProcessDocument {
        /// Identifies the request in its eventual processing result.
        request_id: String,
        /// Provides the original filename for extension-based format detection.
        filename: String,
        /// Contains raw file bytes encoded with base64 for JavaScript Object Notation (JSON) transport.
        data_base64: String,
        /// Selects Tinfoil extraction mode.
        /// Valid values include `"text"`, `"vision"`, `"images"`, `"raw"`, and `"vlm"`.
        /// The daemon validates or defaults this value.
        mode: String,
    },
    /// Requests image analysis through models that accept image input.
    /// The daemon uses `mac-daemon::tinfoil_vision` and redacts on-device personally identifiable information before upload.
    /// This variant is additive under the `PROTOCOL.md` extension policy.
    AnalyzeImage {
        request_id: String,
        /// Contains raw image bytes encoded with base64 for JSON transport.
        image_data_base64: String,
        /// Specifies the question for the image model.
        prompt: String,
    },
    /// Requests audio transcription through Tinfoil.
    /// Send only audio captured from this client's microphone.
    /// Do not send system or speaker output.
    /// Such output can contain other participants' voices without their consent.
    /// See the `tinfoil_audio` module documentation.
    /// This opt-in path sends audio away from the device.
    /// The default `VoiceTranscript` path uses the on-device Speech framework in `VoiceTranscriber.swift`.
    /// This variant is additive under the `PROTOCOL.md` extension policy.
    TranscribeAudio {
        request_id: String,
        /// Contains raw audio bytes encoded with base64 for JSON transport.
        audio_data_base64: String,
        /// Provides an advisory container or codec hint, such as `"wav"` or `"m4a"`.
        /// Tinfoil infers the format from the content.
        format: String,
    },
    /// Requests Tinfoil speech synthesis for `text`.
    /// The daemon uses `mac-daemon::tinfoil_audio::speech`.
    /// This variant is additive under the `PROTOCOL.md` extension policy.
    RequestSpeech {
        request_id: String,
        text: String,
        /// Provides a Tinfoil voice identifier, such as `"serena"`.
        /// The daemon passes this value through unchanged.
        voice: String,
    },
    /// Requests an ordered plan for `goal` through Tinfoil tool calling with `glm-5.2`.
    /// The daemon uses `mac-daemon::tinfoil_planner` outside the task pipeline.
    /// Like [`ClientMessage::ClarifyRequest`], this message does not dispatch work to the desktop backend.
    /// It proposes steps for user review.
    /// Execution requires a separate [`ClientMessage::Prompt`] or [`ClientMessage::RemoteControl`].
    /// This variant is additive under the `PROTOCOL.md` extension policy.
    PlanTask { request_id: String, goal: String },
}

/// Describes one clarifying question from [`ServerMessage::ClarifyQuestions`].
/// The app renders `options` as single-selection suggested answers.
/// It appends a final free-text "Something else…" option.
/// Therefore, an empty `options` list still provides a free-text question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClarifyingQuestion {
    pub question: String,
    pub options: Vec<String>,
}

/// Which mouse button a [`RemoteControlEvent`] refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Right,
}

/// Represents one hands-on action from the app's media stream view.
/// See [`ClientMessage::RemoteControl`].
///
/// Coordinates use the normalized range `0.0..=1.0` within the captured display.
/// The daemon maps them to display points independently of app screen size and video letterboxing.
/// The app does not need the Mac display resolution.
/// A nested `{"action": ...}` tag permits adding an action without changing `ClientMessage` or `ControlMessage` mapping.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RemoteControlEvent {
    /// Starts hands-on control and pauses any active agent turn.
    TakeControl,
    /// Ends hands-on control and resumes the paused turn.
    ReleaseControl,
    /// Moves the cursor to a normalized point.
    Move { x: f64, y: f64 },
    /// Presses or releases a mouse button at a normalized point.
    /// `down: true` presses the button.
    /// A press, one or more `Move` events, and a release perform a drag.
    Button {
        x: f64,
        y: f64,
        button: MouseButton,
        down: bool,
    },
    /// Clicks at the point.
    /// `count: 2` performs a double-click.
    Click {
        x: f64,
        y: f64,
        button: MouseButton,
        count: u32,
    },
    /// Scrolls at the point by `(dx, dy)` wheel deltas in line units.
    /// A negative `dy` scrolls content upward and matches natural touch.
    Scroll { x: f64, y: f64, dx: f64, dy: f64 },
    /// Types text at the current keyboard focus.
    Text { text: String },
    /// Presses or releases a named special key.
    /// Supported examples include `"return"`, `"delete"`, `"escape"`, `"tab"`, and direction names.
    Key { key: String, down: bool },
}

/// Classifies why the daemon requests input through [`ServerMessage::InputRequest`].
/// The five variants match Project Aro PRD requirement P0-14.
///
/// The type serializes as a snake-case string in the `kind` field.
/// For example, `Mfa` serializes as `"kind":"mfa"`.
/// It does not serialize as a nested tagged object.
/// The variants have no associated data.
/// `InputRequest` provides `context` and `response_options`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputRequestKind {
    /// Indicates that work requires a credential, such as a password, application programming interface key, or secret.
    /// Callers must use `context` only to explain the requirement.
    /// They must not put the credential value in `context` or another message field.
    /// A separate `manual_input` channel is designed to carry the value.
    /// This schema does not include that channel.
    Credential,
    /// Indicates that multi-factor authentication (MFA) approval or a code is required.
    Mfa,
    /// Indicates that the agent found multiple plausible actions.
    /// `response_options` contains the available choices.
    AmbiguousChoice,
    /// Indicates that the agent lacks information that it cannot infer or safely estimate.
    /// One example is selecting a calendar account.
    MissingInfo,
    /// Indicates that the next step affects sensitive data or resources.
    /// Examples include financial actions, destructive operations, and private-data access.
    /// The agent requires explicit user consent before continuing.
    SensitiveAccessConsent,
}

impl ClientMessage {
    /// Returns the exact wire `type` discriminant for this variant.
    /// The result matches `#[serde(tag = "type", rename_all = "snake_case")]`.
    /// Examples include `"prompt"`, `"voice_transcript"`, `"stop"`, and `"pin"`.
    /// [`TaskEnvelope::<ClientMessage>::wrap`] uses it for the envelope `message_type` without parsing serialized JSON.
    /// The daemon binary does not call this function because it receives client messages.
    /// Dial-side clients require this public function.
    #[allow(dead_code)]
    pub fn type_tag(&self) -> &'static str {
        match self {
            ClientMessage::TypedPrompt { .. } => "typed_prompt",
            ClientMessage::Prompt { .. } => "prompt",
            ClientMessage::VoiceTranscript { .. } => "voice_transcript",
            ClientMessage::Stop { .. } => "stop",
            ClientMessage::Pause => "pause",
            ClientMessage::Resume => "resume",
            ClientMessage::Redirect { .. } => "redirect",
            ClientMessage::Pin { .. } => "pin",
            ClientMessage::InputResponse { .. } => "input_response",
            ClientMessage::ApprovalResponse { .. } => "approval_response",
            ClientMessage::RemoteControl { .. } => "remote_control",
            ClientMessage::ClarifyRequest { .. } => "clarify_request",
            ClientMessage::ProcessDocument { .. } => "process_document",
            ClientMessage::AnalyzeImage { .. } => "analyze_image",
            ClientMessage::TranscribeAudio { .. } => "transcribe_audio",
            ClientMessage::RequestSpeech { .. } => "request_speech",
            ClientMessage::PlanTask { .. } => "plan_task",
        }
    }
}

/// Represents a control-channel message from the daemon to the app.
///
/// See "ServerMessage" in `holoiroh/PROTOCOL.md` for the wire schema.
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
        #[serde(skip_serializing_if = "Option::is_none", default)]
        execution_mode: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        capabilities: Option<Vec<String>>,
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
    /// Reports a terminal task state.
    /// The envelope `task_id` identifies the turn.
    /// `status` is `"completed"`, `"failed"`, or `"canceled"`.
    /// Clients can distinguish task completion from routine `status` or `task_progress` text.
    /// Previously, a terminal `Done` became a plain `status` line.
    /// The app could not use that line as a reliable end-of-task signal.
    /// This variant is additive under the `PROTOCOL.md` extension policy.
    TaskDone {
        status: String,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        text: Option<String>,
    },
    /// Restores task controls after a reconnect when an earlier task remains active.
    /// The message follows the greeting.
    /// Without it, the app receives only a `Status` line that cannot restore task-control state.
    /// `ControlChannel::accept` emits it for running, paused, or queued work.
    /// `paused` distinguishes active work from work parked before disconnection.
    /// `queued` counts prompts waiting behind the active task.
    /// This variant is additive under the `PROTOCOL.md` extension policy.
    /// Older clients use their generic unrecognized-control-event handling.
    TaskActive {
        #[serde(default)]
        paused: bool,
        #[serde(default)]
        queued: usize,
    },
    /// Reports authentication rejection before the daemon closes the connection.
    /// `ControlChannel::accept` rejects an unknown device with no PIN or an incorrect PIN.
    /// It also rejects sessions that require authentication before authentication is configured.
    /// This variant lets the app show the pairing or PIN screen.
    /// [`ServerMessage::Error`] instead produces a generic error notification.
    AuthRejected {
        #[serde(skip_serializing_if = "Option::is_none", default)]
        text: Option<String>,
    },
    /// Carries the daemon's current `iroh-live:` ticket after the greeting.
    /// The app uses it to replace a stored ticket after daemon identity-key rotation.
    /// The PIN-gated control channel authenticates the daemon that supplies the ticket.
    /// Therefore, the app treats the ticket as authoritative for that machine.
    /// This variant is additive under the `PROTOCOL.md` extension policy.
    /// Older clients use their generic unrecognized-control-event handling.
    CurrentTicket { ticket: String },
    TinfoilVerification {
        host: String,
        ground_truth: serde_json::Value,
    },
    /// Returns questions generated for [`ClientMessage::ClarifyRequest`].
    /// An empty list means that the instruction is clear.
    /// The app then sends the prompt.
    /// The app renders each question's `options` and adds a free-text "Something else…" option.
    /// This variant is additive under the `PROTOCOL.md` extension policy.
    /// Older clients use their generic unrecognized-control-event handling.
    ClarifyQuestions { questions: Vec<ClarifyingQuestion> },
    /// Requests structured user input required by the agent.
    /// Project Aro PRD requirement P0-14 defines this message.
    /// The message describes what the agent needs and why.
    /// It can also provide a closed set of choices.
    /// Callers must not put passwords, MFA codes, API keys, or other secrets in any string field.
    /// The field types do not structurally enforce this prohibition.
    /// A separate `manual_input` channel is designed to carry credential input outside the model context.
    /// This schema does not implement that channel.
    ///
    /// `AmbiguousChoice`, `MissingInfo`, and `SensitiveAccessConsent` accept [`ClientMessage::InputResponse`].
    /// The response must select one `response_options` value.
    /// `Credential` and `Mfa` only announce that out-of-band manual entry is required.
    ///
    /// `ControlChannel::accept` tracks pending requests and enforces `expires_at`.
    /// On expiry, it emits [`ServerMessage::Status`] and safely pauses the task.
    InputRequest {
        /// Identifies the request for a later [`ClientMessage::InputResponse`] or expiry status.
        request_id: String,
        /// Classifies the request with one of the five PRD-defined kinds.
        kind: InputRequestKind,
        /// Explains what the agent needs and why.
        /// Callers must not put a credential value in this field.
        context: String,
        /// Lists the closed set of values that the user can select.
        /// [`ClientMessage::InputResponse::selected_option`] returns one value verbatim.
        /// The list can be empty for `Credential` and `Mfa` requests.
        /// An empty list serializes as `[]` and remains present.
        response_options: Vec<String>,
        /// Sets the expiry time in Unix epoch milliseconds.
        /// See [`Self::input_request`] for the `std::time::SystemTime` calculation.
        expires_at: u64,
    },
    /// Requests approval for one exact irreversible action.
    ApprovalRequest {
        #[serde(flatten)]
        request: ActionApprovalRequest,
    },
    /// Reports whether the focused Mac field is a secure password-class input.
    /// Active examples include login authentication, screen-lock passwords, `sudo`, and Keychain dialogs.
    /// The daemon sends this message only when the state changes.
    /// macOS provides the state through `IsSecureEventInputEnabled()`.
    /// The same condition makes ScreenCaptureKit exclude the field from every captured frame.
    /// WindowServer enforces this security boundary, and processes cannot bypass it.
    /// The app explains that locking limits video and asks the user to enter the password separately.
    /// This explanation replaces an unexplained black rectangle.
    /// This variant is additive under the `PROTOCOL.md` extension policy.
    /// Older clients use their generic unrecognized-control-event handling.
    SecureInputState { active: bool },
    /// Successful reply to [`ClientMessage::ProcessDocument`].
    DocumentProcessed {
        request_id: String,
        /// The converted markdown content.
        markdown: String,
    },
    /// Reports failure for [`ClientMessage::ProcessDocument`].
    /// This dedicated variant lets the app clear the matching attachment's processing indicator.
    /// The app does not need to parse generic [`ServerMessage::Error`] text.
    /// [`ServerMessage::AuthRejected`] establishes the same dedicated-variant pattern.
    DocumentProcessFailed { request_id: String, error: String },
    /// Successful reply to [`ClientMessage::AnalyzeImage`].
    ImageAnalyzed { request_id: String, text: String },
    /// Failure reply to [`ClientMessage::AnalyzeImage`].
    ImageAnalysisFailed { request_id: String, error: String },
    /// Successful reply to [`ClientMessage::TranscribeAudio`].
    AudioTranscribed { request_id: String, text: String },
    /// Failure reply to [`ClientMessage::TranscribeAudio`].
    AudioTranscriptionFailed { request_id: String, error: String },
    /// Returns waveform audio file format (WAV) bytes for [`ClientMessage::RequestSpeech`].
    /// `audio_data_base64` contains the Tinfoil result encoded with base64 for JavaScript Object Notation (JSON) transport.
    SpeechReady {
        request_id: String,
        audio_data_base64: String,
    },
    /// Failure reply to [`ClientMessage::RequestSpeech`].
    SpeechFailed { request_id: String, error: String },
    /// Returns an ordered, fully typed plan for a signed typed prompt.
    TypedPlanReady { request_id: String, plan: Plan },
    /// Reports typed planner lifecycle without overloading general status text.
    PlannerStatus {
        request_id: String,
        status: PlannerRunStatus,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        text: Option<String>,
    },
    /// Reports the terminal binding and outcome for one typed plan action.
    PlannerReceipt { request_id: String, receipt: PlannerReceipt },
    /// Returns ordered human-readable steps for [`ClientMessage::PlanTask`].
    /// The app shows them before any work runs.
    /// See `mac-daemon::tinfoil_planner` for the distinction between planning and execution.
    PlanReady {
        request_id: String,
        steps: Vec<String>,
    },
    /// Failure reply to [`ClientMessage::PlanTask`].
    PlanFailed { request_id: String, error: String },
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
            ServerMessage::TaskDone { .. } => "task_done",
            ServerMessage::TaskActive { .. } => "task_active",
            ServerMessage::AuthRejected { .. } => "auth_rejected",
            ServerMessage::CurrentTicket { .. } => "current_ticket",
            ServerMessage::TinfoilVerification { .. } => "tinfoil_verification",
            ServerMessage::ClarifyQuestions { .. } => "clarify_questions",
            ServerMessage::InputRequest { .. } => "input_request",
            ServerMessage::ApprovalRequest { .. } => "approval_request",
            ServerMessage::SecureInputState { .. } => "secure_input_state",
            ServerMessage::DocumentProcessed { .. } => "document_processed",
            ServerMessage::DocumentProcessFailed { .. } => "document_process_failed",
            ServerMessage::ImageAnalyzed { .. } => "image_analyzed",
            ServerMessage::ImageAnalysisFailed { .. } => "image_analysis_failed",
            ServerMessage::AudioTranscribed { .. } => "audio_transcribed",
            ServerMessage::AudioTranscriptionFailed { .. } => "audio_transcription_failed",
            ServerMessage::SpeechReady { .. } => "speech_ready",
            ServerMessage::SpeechFailed { .. } => "speech_failed",
            ServerMessage::TypedPlanReady { .. } => "typed_plan_ready",
            ServerMessage::PlannerStatus { .. } => "planner_status",
            ServerMessage::PlannerReceipt { .. } => "planner_receipt",
            ServerMessage::PlanReady { .. } => "plan_ready",
            ServerMessage::PlanFailed { .. } => "plan_failed",
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
            execution_mode: None,
            capabilities: None,
        }
    }

    pub fn greeting(
        text: impl Into<String>,
        execution_mode: impl Into<String>,
        capabilities: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        ServerMessage::Status {
            text: Some(text.into()),
            execution_mode: Some(execution_mode.into()),
            capabilities: Some(capabilities.into_iter().map(Into::into).collect()),
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

    /// Convenience constructor for a `task_done` terminal-lifecycle message.
    /// `status` is the snake_case terminal name (`"completed"` / `"failed"`
    /// / `"canceled"`), matching `DoneStatus`'s serde casing on the daemon
    /// side.
    pub fn task_done(status: impl Into<String>, text: Option<String>) -> Self {
        ServerMessage::TaskDone {
            status: status.into(),
            text,
        }
    }

    /// Convenience constructor for an `auth_rejected` message with text.
    pub fn auth_rejected(text: impl Into<String>) -> Self {
        ServerMessage::AuthRejected {
            text: Some(text.into()),
        }
    }

    /// Convenience constructor for a `current_ticket` message carrying the
    /// daemon's live `iroh-live:` ticket.
    pub fn current_ticket(ticket: impl Into<String>) -> Self {
        ServerMessage::CurrentTicket {
            ticket: ticket.into(),
        }
    }

    /// Convenience constructor for a `clarify_questions` message. An empty
    /// `questions` list means the instruction needed no clarification.
    pub fn clarify_questions(questions: Vec<ClarifyingQuestion>) -> Self {
        ServerMessage::ClarifyQuestions { questions }
    }

    /// Creates an `input_request` message with a relative lifetime.
    /// The function adds `ttl` to the current wall-clock time from [`epoch_millis_now`].
    /// The result becomes `expires_at`.
    ///
    /// Callers must use `context` and `response_options` only for request metadata.
    /// Callers must not put credential values or other secrets in these string fields.
    /// The argument types do not structurally enforce this prohibition.
    ///
    /// The daemon has a live sensitive-watchdog trigger for this constructor.
    /// `mac-daemon/examples/input_request_probe.rs` also exercises it directly.
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

    /// Creates the safe-pause `Status` for an expired [`ServerMessage::InputRequest`].
    /// Expiry occurs when no [`ClientMessage::InputResponse`] arrives before the deadline.
    /// See pending-request expiry handling in `ControlChannel::accept`.
    /// Expiry safely pauses the task and is not an error.
    ///
    /// The `ControlChannel::accept` expiry branch routes through `ControlEvent::DaemonStatus`.
    /// The writer task owns the `send` half when expiry occurs.
    /// Therefore, the branch cannot write a `ServerMessage` inline.
    /// Both paths use [`input_request_expired_text`] to produce identical text.
    /// Direct callers and probes can use this public constructor.
    /// `mac-daemon/examples/input_request_probe.rs` exercises it directly.
    #[allow(dead_code)]
    pub fn input_request_expired(request_id: impl Into<String>) -> Self {
        ServerMessage::Status {
            text: Some(input_request_expired_text(&request_id.into())),
            execution_mode: None,
            capabilities: None,
        }
    }
}

/// Returns the current wall-clock time in Unix epoch milliseconds.
/// If the clock precedes [`std::time::UNIX_EPOCH`], the function returns `0`.
/// [`ServerMessage::input_request`] uses it to calculate `expires_at`.
/// `ControlChannel::accept` uses it to check pending-request expiry.
pub fn epoch_millis_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Creates human-readable safe-pause text for an expired [`ServerMessage::InputRequest`].
/// Expiry means that no [`ClientMessage::InputResponse`] arrived.
/// [`ServerMessage::input_request_expired`] uses this function directly.
/// The `ControlChannel::accept` expiry branch uses it through `ControlEvent::DaemonStatus`.
/// The writer task owns `send` when expiry occurs.
/// Sharing this function keeps both paths' text identical.
pub fn input_request_expired_text(request_id: &str) -> String {
    format!(
        "input request {request_id} expired with no response -- task safely paused, waiting for input"
    )
}

/// Serializes `msg` as one newline-delimited JSON line.
/// It writes the line to `send` and then flushes `send`.
/// The accept side uses it for [`ServerMessage`].
/// The dial side uses it for [`ClientMessage`].
/// The generic [`Serialize`] input provides one shared write path.
/// Serialization, write, and flush failures return [`std::io::Error`].
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

/// Reads and deserializes one newline-delimited JSON value as `T`.
///
/// Clean end-of-file returns `Ok(None)`.
/// Blank lines are ignored as harmless keep-alive input.
/// A malformed nonblank line returns `Ok(Some(Err(..)))`.
/// This result keeps parse failures separate from transport failures.
/// The stream remains open, and the caller decides how to respond.
/// For example, it can send [`ServerMessage::error`] and continue reading.
/// An input or output (I/O) failure returns the outer `Err`.
/// See "Error handling on malformed input" in `PROTOCOL.md`.
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
