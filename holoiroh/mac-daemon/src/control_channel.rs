//! Bidirectional control channel.
//!
//! The channel carries small JSON messages between the Mac daemon and the
//! iOS app, alongside the `iroh-live` media broadcast. The channel bridges
//! the messages to [`crate::holo_bridge`].
//!
//! `holoiroh/PROTOCOL.md` defines the schema. Keep the two in sync.
//!
//! ## Relationship to `holoiroh-wire`
//!
//! This module used to define the literal wire-schema types directly.
//! These types now live in the `holoiroh-wire` crate:
//!
//! - [`ClientMessage`] / [`ServerMessage`]
//! - [`TaskEnvelope<T>`]
//! - [`CONTROL_ALPN`]
//! - [`write_line`] / [`read_line`]
//! - [`InboundEnvelopeState::validate_inbound`]
//!
//! This module re-exports them here. See `holoiroh-wire`'s module doc for
//! the exact reason: `ios-bridge` needs these types without pulling in this
//! crate's macOS-only `holo_bridge`/`audit_log` dependencies.
//!
//! This module now owns only the connection-handling logic that uses that
//! schema:
//!
//! - the `iroh` `ProtocolHandler` impl
//! - the PIN/allowlist auth gate
//! - the `iroh::protocol::Router` wiring
//! - per-connection outbound sequence state
//! - the audit-log bookkeeping
//!
//! None of this logic is portable to iOS. It is not even meaningful there:
//! `ios-bridge` is the client side of this protocol, not a second
//! implementation of the daemon's connection handling.
//!
//! ## Relationship to `holo_bridge::control`
//!
//! There are two distinct "control message" concepts in this crate, kept
//! deliberately separate:
//!
//! - **This module** (`control_channel`) owns the literal wire schema:
//!   [`ClientMessage`] / [`ServerMessage`], exactly `{type, text?}` as
//!   documented in `PROTOCOL.md`. This is the schema named in the task
//!   this module was built for. This module also owns the actual `iroh`
//!   transport that carries the schema: ALPN registration,
//!   `accept_bi`/`open_bi`, and NDJSON framing.
//! - [`crate::holo_bridge::control`] owns a richer, internal schema:
//!   `ControlMessage` / `ControlEvent`, correlated by `request_id` and
//!   `context_id`. This schema talks to the `holo serve` A2A bridge. It
//!   does not know about `iroh` or any wire framing at all. See its own
//!   module doc for details.
//!
//! [`ControlChannel`] is the seam between them. For each accepted
//! connection, it does the following:
//!
//! - Decodes wire [`ClientMessage`]s.
//! - Synthesizes a `request_id`.
//! - Forwards a translated [`crate::holo_bridge::control::ControlMessage`]
//!   into a [`crate::holo_bridge::HoloBridge`].
//! - Translates the [`crate::holo_bridge::control::ControlEvent`]s that
//!   come back into wire [`ServerMessage`]s, and writes them back out on
//!   the same stream.
//!
//! This keeps `holo_bridge` transport-agnostic, matching its own docs'
//! intent. It also gives this module a real consumer instead of a
//! dangling internal channel.
//!
//! ## Why a second ALPN, not a second stream multiplexed into the media
//! `Connection`
//!
//! `iroh`'s connection model uses one `iroh::endpoint::Connection` per
//! ALPN. See `iroh::protocol::Router`: it dispatches an incoming
//! connection to a `ProtocolHandler` keyed by the negotiated ALPN. It
//! does not hand out already-open connections for handlers to share.
//!
//! `iroh-live` itself follows this exact pattern. `Live::register_protocols`
//! mounts `iroh_moq::ALPN` (media) and, when gossip is enabled,
//! `iroh_gossip::ALPN`, as two separate ALPNs on the same `iroh::Endpoint` /
//! `iroh::protocol::Router`. See the vendored `iroh-live` source at
//! `iroh-live/src/live.rs::register_protocols`.
//!
//! This module mirrors that idiom. `CONTROL_ALPN` is a third ALPN mounted
//! on the same `Endpoint` via [`ControlChannel::register_protocols`].
//!
//! This module IS "a second logical stream on the same iroh QUIC
//! connection," in the sense the surrounding architecture means it (see
//! `holoiroh/README.md`). It shares the same `iroh::Endpoint`, the same
//! peer `EndpointId`, and the same NAT-punch/relay path and
//! connection-lifecycle/reconnect story as the media broadcast.
//!
//! `iroh` represents "a second logical stream to the same peer" as a
//! second `Connection` object over that shared transport. It does not
//! represent it as a stream nested inside the first `Connection`.
//!
//! Within that one control `Connection`, the actual bidirectional data
//! path is a single QUIC stream. The dial side opens it with
//! [`iroh::endpoint::Connection::open_bi`]. The accept side accepts it
//! with [`iroh::endpoint::Connection::accept_bi`].

use std::sync::Arc;

use anyhow::{Context, Result};
use iroh::{
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler, RouterBuilder},
};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinSet;
use tracing::{debug, info, warn};

use crate::action_executor::{DaemonActionExecutor, ExecutionOutcome};
use crate::agent_loop::{
    AgentLoopError, AgentLoopLimits, AgentLoopOutcome, ObservePlanExecuteLoop,
    TrustedTaskBindings, build_agent_loop,
};
use crate::semantic_ax::{SystemAxError, SystemAxSource};
use crate::tinfoil_planner::TinfoilTurnPlanner;
use crate::allowlist::{Allowlist, is_valid_device_id, verify_pin};
use crate::approval::ApprovalStore;
use crate::audit_log::{
    ActionClass, AppCategory, AuditEntry, AuditLogger, ConnectionPath, FinalStatus,
    InferenceMode, RemoteViewState, now_ms,
};
use crate::execution_mode::ExecutionMode;
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
    ActionApprovalRequest, ActionApprovalResponse, ActionId, ApprovalDecision, ApprovalEffect,
    ApprovalRisk, CONTROL_ALPN, ClientMessage, DEFAULT_EXPIRY_MS, EnvelopeDirection,
    EnvelopeRejection, InboundEnvelopeState, InputRequestKind, MouseButton, PROTOCOL_VERSION,
    RemoteControlEvent, ServerMessage, TaskEnvelope, decode_ed25519_signature,
    encode_ed25519_signature, epoch_millis_now, input_request_expired_text, read_line, write_line,
};

pub const MAX_AUTH_FRAME_BYTES: usize = 4 * 1024;
pub const MAX_CONTROL_FRAME_BYTES: usize = 96 * 1024 * 1024;

/// Identifies a production lifecycle event that invalidates pending approvals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalLifecycleInvalidation<'a> {
    Stop { session_id: &'a str },
    Pause { session_id: &'a str },
    Redirect { session_id: &'a str },
    Disconnect { session_id: &'a str },
    Terminal { task_id: &'a str },
}

/// Applies the same approval invalidation decision as the live control channel.
///
/// This function does not consume an approval or execute an action.
pub fn invalidate_approvals_for_lifecycle(
    store: &mut ApprovalStore,
    invalidation: ApprovalLifecycleInvalidation<'_>,
) -> usize {
    match invalidation {
        ApprovalLifecycleInvalidation::Stop { session_id }
        | ApprovalLifecycleInvalidation::Pause { session_id }
        | ApprovalLifecycleInvalidation::Redirect { session_id }
        | ApprovalLifecycleInvalidation::Disconnect { session_id } => {
            store.cancel_session(session_id)
        }
        ApprovalLifecycleInvalidation::Terminal { task_id } => store.cancel_task_id(task_id),
    }
}
const MAX_TINFOIL_OPERATIONS_PER_CONNECTION: usize = 4;
const TINFOIL_BUSY_ERROR: &str = "too many Tinfoil operations are already running";

type ProductionTypedLoop = ObservePlanExecuteLoop<SystemAxSource, TinfoilTurnPlanner>;

struct PendingTypedContinuation {
    session_id: String,
    task_id: String,
    agent_loop: Arc<tokio::sync::Mutex<ProductionTypedLoop>>,
    permit: tokio::sync::OwnedSemaphorePermit,
}

type TypedContinuationRegistry = Arc<
    std::sync::Mutex<std::collections::HashMap<String, PendingTypedContinuation>>,
>;

type ActiveTypedTaskRegistry = Arc<
    tokio::sync::Mutex<
        std::collections::HashMap<
            String,
            (String, Arc<std::sync::atomic::AtomicBool>, Arc<std::sync::Mutex<()>>),
        >,
    >,
>;
const FRAME_DIGEST_BYTES: usize = 12;
const MAX_LOG_IDENTIFIER_BYTES: usize = 128;
const UNAVAILABLE_LOG_IDENTIFIER: &str = "unavailable";

#[derive(Debug)]
enum FrameReadError {
    Io(std::io::Error),
    InvalidUtf8 {
        byte_count: usize,
        frame_digest: String,
    },
    TooLarge {
        limit: usize,
        frame_digest: String,
    },
}

impl From<std::io::Error> for FrameReadError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

pub fn control_frame_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    data_encoding::HEXLOWER.encode(&digest[..FRAME_DIGEST_BYTES])
}

fn short_device_id(device_id: &str) -> &str {
    device_id.get(..10).unwrap_or(device_id)
}

fn safe_log_identifier(value: Option<&str>) -> String {
    match value {
        Some(value)
            if value.len() <= MAX_LOG_IDENTIFIER_BYTES
                && value.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
                }) =>
        {
            value.to_string()
        }
        _ => UNAVAILABLE_LOG_IDENTIFIER.to_string(),
    }
}

fn envelope_message_id(value: &serde_json::Value) -> String {
    safe_log_identifier(value.get("message_id").and_then(serde_json::Value::as_str))
}

fn payload_request_id(value: &serde_json::Value) -> String {
    let request_id = value
        .get("payload")
        .and_then(|payload| payload.get("request_id"))
        .or_else(|| value.get("request_id"))
        .and_then(serde_json::Value::as_str);
    safe_log_identifier(request_id)
}

fn decode_frame(mut bytes: Vec<u8>) -> std::result::Result<String, FrameReadError> {
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    let byte_count = bytes.len();
    let frame_digest = control_frame_digest(&bytes);
    String::from_utf8(bytes).map_err(|_| FrameReadError::InvalidUtf8 {
        byte_count,
        frame_digest,
    })
}

async fn read_bounded_ndjson_line<R>(
    reader: &mut R,
    limit: usize,
) -> std::result::Result<Option<String>, FrameReadError>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    let mut bytes = Vec::with_capacity(limit.min(8 * 1024));
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                decode_frame(bytes).map(Some)
            };
        }

        let newline = available.iter().position(|byte| *byte == b'\n');
        let content_len = newline.unwrap_or(available.len());
        let consumed = newline.map_or(available.len(), |index| index + 1);
        if content_len > limit.saturating_sub(bytes.len()) {
            let bounded_consumed = limit.saturating_sub(bytes.len()).saturating_add(1);
            let prefix_len = content_len.min(bounded_consumed);
            bytes.extend_from_slice(&available[..prefix_len]);
            let frame_digest = control_frame_digest(&bytes);
            reader.consume(consumed.min(bounded_consumed));
            return Err(FrameReadError::TooLarge {
                limit,
                frame_digest,
            });
        }

        bytes.extend_from_slice(&available[..content_len]);
        reader.consume(consumed);
        if newline.is_some() {
            return decode_frame(bytes).map(Some);
        }
    }
}

fn safe_json_error(error: &serde_json::Error) -> String {
    let category = match error.classify() {
        serde_json::error::Category::Io => "io",
        serde_json::error::Category::Syntax => "syntax",
        serde_json::error::Category::Data => "data",
        serde_json::error::Category::Eof => "eof",
    };
    format!(
        "{category} error at line {} column {}",
        error.line(),
        error.column()
    )
}

fn known_client_message_kind(value: &serde_json::Value) -> &'static str {
    let candidate = value
        .get("payload")
        .and_then(|payload| payload.get("type"))
        .or_else(|| value.get("type"))
        .or_else(|| value.get("message_type"))
        .and_then(serde_json::Value::as_str);
    match candidate {
        Some("prompt") => "prompt",
        Some("voice_transcript") => "voice_transcript",
        Some("stop") => "stop",
        Some("pause") => "pause",
        Some("resume") => "resume",
        Some("redirect") => "redirect",
        Some("pin") => "pin",
        Some("input_response") => "input_response",
        Some("approval_response") => "approval_response",
        Some("remote_control") => "remote_control",
        Some("clarify_request") => "clarify_request",
        Some("process_document") => "process_document",
        Some("analyze_image") => "analyze_image",
        Some("transcribe_audio") => "transcribe_audio",
        Some("request_speech") => "request_speech",
        Some("plan_task") => "plan_task",
        _ => "unknown",
    }
}

pub fn transcription_filename_for_format(
    format: &str,
) -> std::result::Result<&'static str, &'static str> {
    let format = format.trim();
    let aliases: &[(&[&str], &str)] = &[
        (&["wav", "wave", "audio/wav", "audio/x-wav"], "audio.wav"),
        (
            &["m4a", "mp4", "audio/m4a", "audio/x-m4a", "audio/mp4"],
            "audio.m4a",
        ),
        (&["mp3", "audio/mpeg"], "audio.mp3"),
        (&["aac", "audio/aac"], "audio.aac"),
        (&["flac", "audio/flac", "audio/x-flac"], "audio.flac"),
        (&["ogg", "oga", "audio/ogg"], "audio.ogg"),
        (&["webm", "audio/webm"], "audio.webm"),
    ];
    aliases
        .iter()
        .find_map(|(accepted, filename)| {
            accepted
                .iter()
                .any(|alias| format.eq_ignore_ascii_case(alias))
                .then_some(*filename)
        })
        .ok_or("unsupported audio format")
}

/// Per-connection outbound envelope state: this connection's minted
/// `session_id`, plus a monotonic counter for the daemon's own outbound
/// `sequence_number`s.
///
/// [`ProtocolHandler::accept`]'s writer task (`send_task`) owns this state
/// entirely. This state stays separate from [`InboundEnvelopeState`]
/// rather than combined into one struct, because the two live in
/// genuinely different places. The writer task owns `send` for the
/// connection's whole lifetime and needs this state moved into it. The
/// read loop needs `InboundEnvelopeState` mutably available on every line
/// it reads.
///
/// A single shared struct would force the read loop and writer task to
/// fight over one lock for two logically-independent counters. Inbound
/// sequence tracking has nothing to do with outbound sequence numbering --
/// see `next_outbound_sequence`'s own doc.
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
    /// [`TaskEnvelope`] for this connection. It advances the counter.
    ///
    /// This counter is independent of inbound sequence tracking. The
    /// daemon's own outbound stream is numbered separately from whatever
    /// the peer sends, because the two are different logical sequences.
    /// The envelope is scoped per `session_id` per direction, not by a
    /// single shared counter.
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

fn sign_daemon_envelope<T: serde::Serialize>(
    envelope: &mut TaskEnvelope<T>,
    signer: &iroh::SecretKey,
    recipient: &iroh::PublicKey,
) -> Result<()> {
    let signer_public = signer.public();
    let payload = envelope
        .signing_payload(
            EnvelopeDirection::DaemonToClient,
            signer_public.as_bytes(),
            recipient.as_bytes(),
        )
        .context("building daemon envelope signing payload")?;
    let signature = signer.sign(&payload);
    envelope.signature = Some(encode_ed25519_signature(&signature.to_bytes()));
    Ok(())
}

fn verify_client_envelope<T: serde::Serialize>(
    envelope: &TaskEnvelope<T>,
    signer: &iroh::PublicKey,
    recipient: &iroh::PublicKey,
) -> std::result::Result<(), String> {
    let encoded = envelope
        .signature
        .as_deref()
        .ok_or_else(|| "signature is required".to_string())?;
    let bytes = decode_ed25519_signature(encoded)
        .map_err(|error| format!("invalid signature encoding: {error}"))?;
    let signature = iroh::Signature::from_bytes(&bytes);
    let payload = envelope
        .signing_payload(
            EnvelopeDirection::ClientToDaemon,
            signer.as_bytes(),
            recipient.as_bytes(),
        )
        .map_err(|error| format!("invalid signing payload: {error}"))?;
    signer
        .verify(&payload, &signature)
        .map_err(|_| "signature verification failed".to_string())
}

#[doc(hidden)]
#[allow(dead_code)]
pub fn sign_daemon_envelope_for_probing<T: serde::Serialize>(
    envelope: &mut TaskEnvelope<T>,
    signer: &iroh::SecretKey,
    recipient: &iroh::PublicKey,
) -> Result<()> {
    sign_daemon_envelope(envelope, signer, recipient)
}

#[doc(hidden)]
#[allow(dead_code)]
pub fn verify_client_envelope_for_probing<T: serde::Serialize>(
    envelope: &TaskEnvelope<T>,
    signer: &iroh::PublicKey,
    recipient: &iroh::PublicKey,
) -> std::result::Result<(), String> {
    verify_client_envelope(envelope, signer, recipient)
}

pub fn admit_post_signature_envelope<T: serde::Serialize>(
    envelope: &TaskEnvelope<T>,
    message: &ClientMessage,
    expected_session_id: &str,
    first_inbound_envelope: bool,
    inbound_state: &mut InboundEnvelopeState,
    execution_mode: ExecutionMode,
    executor: &Arc<std::sync::Mutex<DaemonActionExecutor>>,
    now: u64,
) -> std::result::Result<(), String> {
    if envelope.protocol_version != PROTOCOL_VERSION {
        return Err("protocol version mismatch".to_owned());
    }
    if envelope.message_type != message.type_tag() {
        return Err("message type mismatch".to_owned());
    }
    if envelope.session_id != expected_session_id {
        return Err("session binding mismatch".to_owned());
    }
    if first_inbound_envelope && envelope.sequence_number != 0 {
        return Err("first sequence must be zero".to_owned());
    }
    inbound_state
        .validate_inbound(envelope)
        .map_err(|error| error.to_string())?;
    if !execution_mode.admits(message) {
        return Err("message rejected by execution mode".to_owned());
    }
    match message {
        ClientMessage::TypedPrompt { .. } if envelope.task_id.is_none() => {
            return Err("typed prompt requires a signed task binding".to_owned());
        }
        ClientMessage::ApprovalResponse { response } => route_validated_approval_response(
            response,
            &envelope.session_id,
            envelope.task_id.as_deref(),
            executor,
            now,
        )?,
        _ => {}
    }
    Ok(())
}

pub fn build_production_typed_loop<P>(
    executor: Arc<std::sync::Mutex<DaemonActionExecutor>>,
    planner: P,
    limits: AgentLoopLimits,
) -> crate::agent_loop::ObservePlanExecuteLoop<SystemAxSource, P> {
    build_agent_loop(executor, SystemAxSource, planner, limits)
}

fn typed_publication_allowed(
    active_session: &str,
    expected_session: &str,
    canceled: &std::sync::atomic::AtomicBool,
) -> bool {
    active_session == expected_session
        && !canceled.load(std::sync::atomic::Ordering::Acquire)
}

#[doc(hidden)]
pub fn typed_publication_allowed_for_probing(
    active_session: &str,
    expected_session: &str,
    canceled: &std::sync::atomic::AtomicBool,
) -> bool {
    typed_publication_allowed(active_session, expected_session, canceled)
}

async fn publish_typed_loop_result(
    result: std::result::Result<AgentLoopOutcome, AgentLoopError<anyhow::Error, SystemAxError>>,
    session_id: String,
    request_id: String,
    agent_loop: Arc<tokio::sync::Mutex<ProductionTypedLoop>>,
    permit: tokio::sync::OwnedSemaphorePermit,
    continuations: TypedContinuationRegistry,
    active_tasks: ActiveTypedTaskRegistry,
    tx: mpsc::UnboundedSender<ControlEvent>,
) {
    if !matches!(&result, Ok(AgentLoopOutcome::ApprovalRequired { .. })) {
        active_tasks.lock().await.remove(&request_id);
    }
    match result {
        Ok(AgentLoopOutcome::Completed { .. }) => {
            let _ = tx.send(ControlEvent::Done {
                request_id,
                context_id: None,
                status: DoneStatus::Completed,
                message: None,
            });
        }
        Ok(AgentLoopOutcome::ApprovalRequired { receipt, .. }) => {
            let remaining = agent_loop.lock().await.remaining();
            let publication = active_tasks
                .lock()
                .await
                .get(&request_id)
                .and_then(|(active_session, canceled, execution_gate)| {
                    (active_session == &session_id).then(|| {
                        (
                            active_session.clone(),
                            canceled.clone(),
                            execution_gate.clone(),
                        )
                    })
                });
            let Some((active_session, canceled, execution_gate)) = publication else {
                return;
            };
            let _publication = execution_gate
                .lock()
                .expect("typed execution gate lock poisoned");
            if !typed_publication_allowed(&active_session, &session_id, &canceled) {
                return;
            }
            if let ExecutionOutcome::ApprovalRequired(request) = receipt.outcome {
                let approval_id = request.approval_id.clone();
                continuations
                    .lock()
                    .expect("typed continuation registry lock poisoned")
                    .insert(
                    approval_id.clone(),
                    PendingTypedContinuation {
                        session_id: session_id.clone(),
                        task_id: request_id.clone(),
                        agent_loop,
                        permit,
                    },
                );
                let _ = tx.send(ControlEvent::ApprovalRequested {
                    request_id: request_id.clone(),
                    request,
                });
                let deadline_continuations = continuations.clone();
                let deadline_active_tasks = active_tasks.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(remaining).await;
                    let Some(pending) = deadline_continuations
                        .lock()
                        .expect("typed continuation registry lock poisoned")
                        .remove(&approval_id)
                    else {
                        return;
                    };
                    if let Some((_, canceled, execution_gate)) =
                        deadline_active_tasks.lock().await.remove(&request_id)
                    {
                        let _execution = execution_gate
                            .lock()
                            .expect("typed execution gate lock poisoned");
                        canceled.store(true, std::sync::atomic::Ordering::Release);
                    }
                    pending
                        .agent_loop
                        .lock()
                        .await
                        .executor()
                        .lock()
                        .expect("typed_action_executor lock poisoned")
                        .cancel_approval(&approval_id);
                    let _ = tx.send(ControlEvent::Done {
                        request_id,
                        context_id: None,
                        status: DoneStatus::Failed,
                        message: Some("typed planner deadline reached".to_string()),
                    });
                });
            } else {
                let _ = tx.send(ControlEvent::Error {
                    request_id,
                    message: "typed executor returned an invalid approval receipt".to_string(),
                });
            }
        }
        Ok(AgentLoopOutcome::Rejected { .. }) => {
            let _ = tx.send(ControlEvent::Done {
                request_id,
                context_id: None,
                status: DoneStatus::Failed,
                message: Some("typed action rejected".to_string()),
            });
        }
        Ok(AgentLoopOutcome::Canceled { .. }) => {
            let _ = tx.send(ControlEvent::Done {
                request_id,
                context_id: None,
                status: DoneStatus::Canceled,
                message: Some("typed planner canceled".to_string()),
            });
        }
        Ok(AgentLoopOutcome::StepLimit) => {
            let _ = tx.send(ControlEvent::Done {
                request_id,
                context_id: None,
                status: DoneStatus::Failed,
                message: Some("typed planner step limit reached".to_string()),
            });
        }
        Ok(AgentLoopOutcome::Deadline) => {
            let _ = tx.send(ControlEvent::Done {
                request_id,
                context_id: None,
                status: DoneStatus::Failed,
                message: Some("typed planner deadline reached".to_string()),
            });
        }
        Err(error) => {
            let message: String = format!("{error:?}").chars().take(1024).collect();
            let _ = tx.send(ControlEvent::Error {
                request_id: request_id.clone(),
                message,
            });
            let _ = tx.send(ControlEvent::Done {
                request_id,
                context_id: None,
                status: DoneStatus::Failed,
                message: Some("typed planner failed".to_string()),
            });
        }
    }
}

async fn cancel_typed_continuations(
    continuations: &TypedContinuationRegistry,
    active_tasks: &ActiveTypedTaskRegistry,
    session_id: &str,
    task_id: Option<&str>,
) {
    {
        let mut active = active_tasks.lock().await;
        let task_ids: Vec<_> = active
            .iter()
            .filter_map(|(active_task_id, (active_session_id, _, _))| {
                (active_session_id == session_id
                    && task_id.is_none_or(|task_id| active_task_id == task_id))
                    .then(|| active_task_id.clone())
            })
            .collect();
        for active_task_id in task_ids {
            if let Some((_, canceled, execution_gate)) = active.remove(&active_task_id) {
                let _execution = execution_gate
                    .lock()
                    .expect("typed execution gate lock poisoned");
                canceled.store(true, std::sync::atomic::Ordering::Release);
            }
        }
    }
    let mut registry = continuations
        .lock()
        .expect("typed continuation registry lock poisoned");
    let approval_ids: Vec<_> = registry
        .iter()
        .filter_map(|(approval_id, pending)| {
            (pending.session_id == session_id
                && task_id.is_none_or(|task_id| pending.task_id == task_id))
                .then(|| approval_id.clone())
        })
        .collect();
    for approval_id in approval_ids {
        registry.remove(&approval_id);
    }
}

pub fn route_validated_approval_response(
    response: &ActionApprovalResponse,
    session_id: &str,
    task_id: Option<&str>,
    executor: &Arc<std::sync::Mutex<DaemonActionExecutor>>,
    now: u64,
) -> std::result::Result<(), String> {
    let store = executor
        .lock()
        .map_err(|_| "typed action executor lock poisoned".to_owned())?
        .approval_store();
    let result = store
        .lock()
        .map_err(|_| "approval store lock poisoned".to_owned())?
        .route_response(response, session_id, task_id, now);
    result.map_err(|error| format!("approval response rejected: {error:?}"))
}

/// Translates a [`crate::holo_bridge::control::ControlEvent`] (the
/// internal, `request_id`/`context_id`-correlated bridge schema) down
/// to the minimal wire [`ServerMessage`] schema. This module's
/// `PROTOCOL.md` defines that wire schema.
///
/// The wire schema does not include the correlation ids themselves. The
/// task's literal ask has no fields for them. This function folds the ids
/// into human-readable `text` instead of dropping them silently. A future
/// `PROTOCOL.md` revision may promote them to real fields. See
/// `PROTOCOL.md`'s "Future extension" section.
///
/// This is a free function, not an inherent `impl ServerMessage` method
/// (which is how the code originally worked). `ServerMessage` now lives
/// in `holoiroh-wire`. Rust's orphan rule forbids `impl`ing inherent
/// methods on a foreign type from this crate. This function also depends
/// on `ControlEvent`/`DoneStatus` (`crate::holo_bridge`'s internal,
/// non-wire, desktop-side schema); `holoiroh-wire` exists specifically to
/// keep that kind of dependency out of the wire-schema crate.
///
/// Call sites now read `control_channel::from_control_event(event)`
/// instead of `ServerMessage::from_control_event(event)`. These call
/// sites are this module's own writer task, plus
/// `examples/control_channel_probe.rs`. The behavior is the same; only
/// the call syntax changed.
///
/// ## Not wired to [`crate::task_state::TaskState`]
///
/// [`crate::task_state::TaskState`] is this crate's Project Aro PRD
/// task-lifecycle enum (created/queued/connecting/.../completed, plus
/// interactive-wait and terminal states). This function deliberately
/// does not thread `TaskState` in.
///
/// [`ControlEvent::Queued`] below is the one variant here with any real
/// per-state correspondence. It already has a byte-exact wire string
/// (`"queued, N ahead"`) asserted by `examples/control_channel_probe.rs`.
/// Embedding `TaskState`'s serialized value into it would be a breaking,
/// unrequested wire change, not a natural hook.
///
/// Every other arm carries either free text or the unrelated 3-way
/// `DoneStatus`, with no correspondence to `TaskState`'s finer
/// granularity. `holo_bridge::a2a_client`'s `TaskUpdate`
/// (`Working`/`Answer`/`Terminal`) is the actual upstream event source,
/// and it does not report which fine-grained lifecycle state a task is
/// in.
///
/// The next task that gives this bridge a fine-grained event source (for
/// example, `holo-desktop-cli` trajectory events that name a specific
/// step) should wire `TaskState` in here.
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
        // Live auto-yield pause/resume -> the same wire message the reconnect
        // path uses, so the phone's Pause/Stop pill updates in real time.
        ControlEvent::TaskActive { paused, queued } => ServerMessage::TaskActive { paused, queued },
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
        ControlEvent::ClarifyQuestions { questions } => ServerMessage::clarify_questions(questions),
        ControlEvent::SecureInputState { active } => ServerMessage::SecureInputState { active },
        ControlEvent::ApprovalRequested { request, .. } => {
            ServerMessage::ApprovalRequest { request }
        }
        ControlEvent::DocumentProcessed {
            request_id,
            markdown,
        } => ServerMessage::DocumentProcessed {
            request_id,
            markdown,
        },
        ControlEvent::DocumentProcessFailed { request_id, error } => {
            ServerMessage::DocumentProcessFailed { request_id, error }
        }
        ControlEvent::ImageAnalyzed { request_id, text } => {
            ServerMessage::ImageAnalyzed { request_id, text }
        }
        ControlEvent::ImageAnalysisFailed { request_id, error } => {
            ServerMessage::ImageAnalysisFailed { request_id, error }
        }
        ControlEvent::AudioTranscribed { request_id, text } => {
            ServerMessage::AudioTranscribed { request_id, text }
        }
        ControlEvent::AudioTranscriptionFailed { request_id, error } => {
            ServerMessage::AudioTranscriptionFailed { request_id, error }
        }
        ControlEvent::SpeechReady {
            request_id,
            audio_data_base64,
        } => ServerMessage::SpeechReady {
            request_id,
            audio_data_base64,
        },
        ControlEvent::SpeechFailed { request_id, error } => {
            ServerMessage::SpeechFailed { request_id, error }
        }
        ControlEvent::PlanReady { request_id, steps } => {
            ServerMessage::PlanReady { request_id, steps }
        }
        ControlEvent::PlanFailed { request_id, error } => {
            ServerMessage::PlanFailed { request_id, error }
        }
    }
}

/// The `request_id` a [`ControlEvent`] itself carries, when it names a
/// real one. The writer task uses this to stamp the correct envelope
/// `task_id` on each outbound event.
///
/// Before turns were spawned off the read loop, the last-inbound-envelope's
/// task_id was a safe stand-in, because only one turn ran at a time in
/// strict request/response order. With concurrent turns, an event must
/// correlate by its own id. Otherwise, for example, a mid-turn `Stop`'s
/// inbound envelope would re-stamp the still-streaming prompt's progress
/// events with the stop's task_id.
fn event_request_id(event: &ControlEvent) -> Option<String> {
    let id = match event {
        ControlEvent::Ack { request_id }
        | ControlEvent::Progress { request_id, .. }
        | ControlEvent::Answer { request_id, .. }
        | ControlEvent::Done { request_id, .. }
        | ControlEvent::Error { request_id, .. }
        | ControlEvent::Queued { request_id, .. }
        | ControlEvent::InputRequested { request_id, .. }
        | ControlEvent::ApprovalRequested { request_id, .. }
        | ControlEvent::DocumentProcessed { request_id, .. }
        | ControlEvent::DocumentProcessFailed { request_id, .. }
        | ControlEvent::ImageAnalyzed { request_id, .. }
        | ControlEvent::ImageAnalysisFailed { request_id, .. }
        | ControlEvent::AudioTranscribed { request_id, .. }
        | ControlEvent::AudioTranscriptionFailed { request_id, .. }
        | ControlEvent::SpeechReady { request_id, .. }
        | ControlEvent::SpeechFailed { request_id, .. }
        | ControlEvent::PlanReady { request_id, .. }
        | ControlEvent::PlanFailed { request_id, .. } => request_id,
        ControlEvent::DaemonStatus { .. }
        | ControlEvent::TaskActive { .. }
        | ControlEvent::ClarifyQuestions { .. }
        | ControlEvent::SecureInputState { .. } => return None,
    };
    if id.is_empty() { None } else { Some(id.clone()) }
}

/// Logs one [`crate::audit_log::CloudEgressEntry`]. On failure, it warns
/// and never propagates the error. This matches [`AuditLogger::append`]'s
/// documented best-effort posture for its other caller: a logging failure
/// never tears down the in-flight turn that produced the entry.
///
/// Shared by all five document/image/audio/planner spawn blocks in the
/// read loop below.
fn log_cloud_egress(
    audit: &AuditLogger,
    request_id: &str,
    capability: crate::audit_log::CloudEgressCapability,
    success: bool,
    byte_count: u64,
) {
    let entry = crate::audit_log::CloudEgressEntry {
        request_id: request_id.to_string(),
        capability,
        occurred_at_ms: now_ms(),
        success,
        byte_count,
    };
    if let Err(err) = audit.append(&entry) {
        warn!(error = %err, "control channel: failed to append cloud-egress audit entry");
    }
}

/// Converts a wire [`ClientMessage`] plus a synthesized `request_id` into
/// the internal [`ControlMessage`] shape [`crate::holo_bridge::HoloBridge`]
/// expects. The wire schema has no `context_id`: each `ClientMessage`
/// carries no session-continuity field, per `PROTOCOL.md`. So every
/// message starts a fresh `holo serve` A2A context. A later change can
/// layer on per-connection conversation continuity, by threading a
/// connection-scoped `context_id` through here, without any wire-format
/// change.
///
/// Returns `None` for [`ClientMessage::Pin`]. [`ControlChannel::authenticate`]'s
/// gate consumes that variant entirely, before the main accept loop
/// (below) ever calls this function. A `Pin` arriving mid-stream, after
/// auth already succeeded, has no `HoloBridge` equivalent to translate
/// to. So the accept loop acks it locally instead of forwarding it. See
/// the `Ok(ClientMessage::Pin { .. })` arm in [`ProtocolHandler::accept`].
///
/// Also returns `None` for [`ClientMessage::InputResponse`]. That variant
/// answers a pending [`ServerMessage::InputRequest`] the accept loop
/// itself is tracking, by matching `request_id` against the outstanding
/// request and clearing its expiry timer. `HoloBridge`'s A2A-oriented
/// `ControlMessage` has no equivalent shape for this today.
///
/// This function is `pub`, not private, specifically so
/// `examples/holo_stop_probe.rs` can assert the exact
/// wire-[`ClientMessage::Stop`] -> internal-[`ControlMessage::Stop`]
/// mapping directly. `examples/holo_stop_probe.rs` is the run-by-hand
/// witness for the remote kill-switch path, per this repo's no-unit-tests
/// rule. It reaches this mapping without spinning up a live `iroh`
/// connection through [`ProtocolHandler::accept`]. That accept loop still
/// calls this function internally, so it is not dead code from the bin
/// target's perspective.
///
/// Injection carries ABSOLUTE cursor positions. So an out-of-order apply
/// teleports the pointer backwards rather than merely arriving late.
/// These variants are synchronous CGEvent posts. The caller must await
/// them in stream order. `TakeControl`/`ReleaseControl` take the pause
/// path. They are deliberately excluded here so they keep the spawn.
pub fn must_preserve_arrival_order(message: &ControlMessage) -> bool {
    matches!(
        message,
        ControlMessage::RemoteControl {
            event: RemoteControlEvent::Move { .. }
                | RemoteControlEvent::Button { .. }
                | RemoteControlEvent::Click { .. }
                | RemoteControlEvent::Scroll { .. }
                | RemoteControlEvent::Text { .. }
                | RemoteControlEvent::Key { .. }
        }
    )
}

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
        ClientMessage::Stop { context_id } => Some(ControlMessage::Stop {
            request_id,
            context_id,
            force: false,
        }),
        ClientMessage::Pause => Some(ControlMessage::Pause { request_id }),
        ClientMessage::Resume => Some(ControlMessage::Resume { request_id }),
        ClientMessage::Redirect { text } => Some(ControlMessage::Redirect { request_id, text }),
        ClientMessage::RemoteControl { event } => Some(ControlMessage::RemoteControl { event }),
        ClientMessage::Pin { .. } => None,
        ClientMessage::InputResponse { .. } | ClientMessage::ApprovalResponse { .. } => None,
        // Clarification runs off the desktop-task pipeline (handled inline in
        // the control-channel read loop), so it never becomes a ControlMessage.
        ClientMessage::ClarifyRequest { .. } => None,
        // Handled entirely by their own arms in the read loop below, off the desktop-task
        // pipeline (same as ClarifyRequest) -- listed only to keep this match exhaustive.
        ClientMessage::TypedPrompt { .. }
        | ClientMessage::ProcessDocument { .. }
        | ClientMessage::AnalyzeImage { .. }
        | ClientMessage::TranscribeAudio { .. }
        | ClientMessage::RequestSpeech { .. }
        | ClientMessage::PlanTask { .. } => None,
    }
}

/// One [`ServerMessage::InputRequest`] a connection is currently waiting
/// on. The connection waits for a [`ClientMessage::InputResponse`], or for
/// the request to expire.
///
/// [`ControlChannel::accept`] tracks at most one of these per connection at
/// a time. This matches `HoloControlBridge`'s existing single-active-turn
/// concurrency model (`busy`/`queue` in `holo_bridge::control`): a turn
/// that needs user input pauses that one turn. A single control-channel
/// connection does not need multiple simultaneous outstanding input
/// requests today. Tracking more than one would need its own
/// bounded-queue design this row does not need to solve. A future
/// multi-outstanding-request design would replace this `Option` with a
/// keyed map, but nothing in this daemon currently produces more than one
/// at a time.
///
/// Fields are private. In real use, only [`ControlChannel::accept`]'s
/// internal bookkeeping constructs this struct. [`Self::for_probing`] is
/// the one exception, mirroring [`AuthState::for_probing`]'s rationale
/// exactly: `examples/input_request_probe.rs` needs to build one directly
/// to witness [`wait_for_expiry`]'s real timing behavior, without spinning
/// up a live `iroh` connection.
pub struct PendingInputRequest {
    request_id: String,
    /// Epoch millis, same unit as [`ServerMessage::InputRequest::expires_at`].
    /// This value is copied here, rather than re-derived from a stored
    /// `ServerMessage`, so [`wait_for_expiry`] only needs this one `u64` to
    /// compute the sleep duration.
    expires_at: u64,
}

impl PendingInputRequest {
    /// Builds a `PendingInputRequest` directly for
    /// `examples/input_request_probe.rs` (see struct doc). `main.rs`'s
    /// binary path does not call this function, the same status as
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
/// passed. Never resolves at all if `pending` is `None`. This lets code
/// use this function directly as one arm of `tokio::select!` in
/// [`ControlChannel::accept`]'s connection loop, without that arm ever
/// firing spuriously when no request is outstanding.
///
/// This function computes the sleep duration from real wall-clock time
/// ([`epoch_millis_now`]) on every poll, rather than once up front. So a
/// deadline that is already in the past, or that arrives while this
/// future is first constructed, resolves on the very next `.await` point
/// instead of through any special-cased branch. `Duration::ZERO` sleeps
/// resolve immediately, which is exactly the desired "already expired ->
/// safe-pause right away" behavior for a degenerate past-`expires_at`
/// request.
///
/// This function is `pub`, not private, so `examples/input_request_probe.rs`
/// can race real `tokio::time` against a real [`PendingInputRequest`] the
/// same way [`ControlChannel::accept`]'s own `tokio::select!` does. This
/// is the same probe-access rationale as [`ControlChannel::authenticate`].
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

/// Audit-log start metadata for one in-flight task. The main
/// [`ProtocolHandler::accept`] loop records this at dispatch time. The
/// `send_task` spawned in that same function consumes it when the
/// matching [`ControlEvent::Done`] arrives. See the audit-log bookkeeping
/// comment where `accept` constructs `audit_starts`, for why this is
/// split across the two tasks.
struct AuditTaskStart {
    started_at_ms: u64,
    action_class: ActionClass,
}

/// Applies one [`ControlEvent`] to the running audit-log bookkeeping for
/// its connection. It tallies `Progress` events into `action_counts`. On
/// `Done`, it looks up and removes the matching [`AuditTaskStart`]
/// recorded at dispatch time, then builds and appends a complete
/// [`AuditEntry`] via [`AuditLogger::append`]. This is the "one entry
/// when a task completes" half of this module's P0-12 wiring. The "one
/// entry when a task starts" half is the `audit_starts.lock()...insert(..)`
/// call in `accept` itself.
///
/// A `request_id` with no matching `audit_starts` entry is silently
/// skipped: nothing to close out, not an error. This happens for any
/// `ClientMessage` kind other than `Prompt`/`VoiceTranscript` -- see that
/// insert's own doc for which kinds get a start record.
///
/// This is a free function, not a `ControlChannel` method, because it
/// needs to run inside `send_task`'s spawned `async move` block, which
/// does not hold `&self`. It takes every piece of state it needs as an
/// explicit parameter instead.
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
/// This state is held behind `std::sync::Mutex`, not `tokio::sync::Mutex`.
/// Every access is a short, synchronous critical section: a `HashSet`/`Vec`
/// lookup, or a JSON file write on the rare add-device path, with no
/// `.await` inside the lock. So a std lock is both correct and cheaper
/// than an async one here. `HoloControlBridge::events_tx` uses the same
/// reasoning for its own `std::sync::RwLock` (see that type's doc
/// comment).
pub struct AuthState {
    allowlist: Allowlist,
    allowlist_path: std::path::PathBuf,
    /// The PIN this daemon process generated at startup. `None` means PIN
    /// auth is disabled for this run (see [`ControlChannel::new`] /
    /// [`ControlChannel::with_auth`]). Every connection is then gated on
    /// the allowlist alone, and an unknown device is rejected outright
    /// with no PIN-entry path offered. This matches "reject
    /// unknown/wrong-PIN connections": with no PIN configured, there is
    /// no correct PIN to enter, so unknown devices are simply rejected.
    expected_pin: Option<String>,
}

impl AuthState {
    /// Constructs an `AuthState` directly, bypassing the real
    /// `~/.holoiroh/allowlist.json` load that `ControlChannel::new`/`with_auth`
    /// normally perform. This function is `pub`, not only reachable via
    /// those constructors, specifically so `examples/auth_gate_probe.rs`
    /// can exercise the actual gate function against a real in-memory
    /// `AuthState` and a bounded `AsyncBufRead` reader.
    /// `examples/auth_gate_probe.rs` is a real, run-by-hand live witness
    /// for [`ControlChannel::authenticate`]'s PIN/allowlist gate logic
    /// (see this repo's no-unit-tests rule). It uses the same seam the
    /// removed `#[tokio::test]` async tests used, just driven by
    /// `cargo run` instead of `cargo test`. `main.rs`'s binary target does
    /// not call this function; it builds real `AuthState` only via
    /// `ControlChannel::new`/`with_auth`'s real allowlist load. This
    /// function carries `#[allow(dead_code)]` there, the same status as
    /// `allowlist.rs`'s own probe-only convenience methods.
    #[allow(dead_code)]
    pub fn for_probing(
        expected_pin: Option<&str>,
        pre_allowed: &[&str],
        allowlist_path: std::path::PathBuf,
    ) -> Self {
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

    /// True if `device_id` is currently allowlisted. The probe uses this
    /// to confirm `authenticate`'s side effect actually happened: adding
    /// a newly PIN-verified device. This has the same
    /// not-called-from-`main.rs` status as [`Self::for_probing`].
    #[allow(dead_code)]
    pub fn contains_key(&self, device_id: &str) -> bool {
        self.allowlist.contains_key(device_id)
    }
}

/// Handle to the control channel. It mounts [`CONTROL_ALPN`] on the
/// shared `iroh` `Endpoint`/`Router` (accept side). It also lets the
/// daemon open the matching stream when dialing a peer (dial side).
///
/// Each accepted connection first runs through the auth gate documented
/// on [`ProtocolHandler::accept`] below (allowlist + first-connection
/// PIN). Only a connection that passes is forwarded into the shared
/// [`HoloBridge`]. That connection then gets its
/// [`crate::holo_bridge::control::ControlEvent`]s streamed back out as
/// [`ServerMessage`]s on the same stream.
#[derive(Clone)]
pub struct ControlChannel {
    bridge: Arc<HoloBridge>,
    execution_mode: ExecutionMode,
    /// The same persisted identity key owned by the shared Live endpoint.
    /// It signs every post-auth daemon envelope; no second identity exists.
    signing_key: Arc<iroh::SecretKey>,
    auth: Arc<std::sync::Mutex<AuthState>>,
    /// Metadata-only local audit log (Project Aro PRD row P0-12) -- see
    /// `crate::audit_log`'s module doc for exactly what is and isn't
    /// recorded. This field is `Arc`, not owned, because `ControlChannel`
    /// is itself cheaply `Clone`d per accepted connection (see this
    /// struct's own existing `bridge`/`auth` fields), and every clone
    /// must append to the same underlying file.
    audit: Arc<AuditLogger>,
    /// This daemon's own drift-proof (node-id-only) `iroh-live:` ticket.
    /// The daemon sends this to the peer as a
    /// [`ServerMessage::CurrentTicket`] right after the greeting, so a
    /// client can refresh a stored default whose ticket went stale on
    /// identity rotation. This field is `Arc<str>` for the same
    /// per-connection cheap-clone reason as the fields above.
    current_ticket: Arc<str>,
    /// Clarifying-questions inference backend (Tinfoil key + model), when
    /// a `TINFOIL_API_KEY` is configured. `None` disables clarification
    /// entirely. A `ClarifyRequest` then replies with an empty question
    /// set, so the app proceeds with a direct send.
    clarify: Option<crate::clarify::ClarifyConfig>,
    /// The raw Tinfoil bearer key, shared by the document/image/audio/planner
    /// handlers below. Each of those modules takes the key directly,
    /// rather than a per-module config struct, unlike `clarify`'s
    /// `ClarifyConfig`. No per-module model override env var exists for
    /// any of them yet, so a bare key is the whole config. `None`
    /// disables all four features. Each then replies with its own
    /// `*Failed` event stating no key is configured, mirroring
    /// `clarify`'s empty-questions-when-disabled posture rather than
    /// silently hanging.
    tinfoil_client: Option<Arc<crate::tinfoil_client::TinfoilClient>>,
    approval_store: Arc<std::sync::Mutex<ApprovalStore>>,
    typed_action_executor: Arc<std::sync::Mutex<DaemonActionExecutor>>,
}

impl std::fmt::Debug for ControlChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControlChannel").finish_non_exhaustive()
    }
}

impl ControlChannel {
    /// Creates a new control channel wrapping an already-started
    /// [`HoloBridge`], with no auth enforced. It loads the allowlist from
    /// [`Allowlist::default_path`], best-effort: a load failure logs a
    /// warning and starts from an empty in-memory allowlist, rather than
    /// failing daemon startup. Every device is treated as effectively
    /// allowlisted, with no PIN and no rejection, until code uses
    /// [`Self::with_auth`] instead.
    ///
    /// This is the zero-friction default for local dev/testing. It
    /// matches this crate's existing "best-effort, degrade don't crash"
    /// posture -- see `main.rs`'s `holo_bridge` startup handling. A real
    /// deployment should not call this constructor. See `PAIRING.md`'s
    /// "Exact remaining wiring step" for what `main.rs` would need to
    /// change to actually enable enforcement by default.
    pub fn new(
        bridge: Arc<HoloBridge>,
        execution_mode: ExecutionMode,
        signing_key: Arc<iroh::SecretKey>,
        audit: Arc<AuditLogger>,
        current_ticket: Arc<str>,
        clarify: Option<crate::clarify::ClarifyConfig>,
        tinfoil_client: Option<Arc<crate::tinfoil_client::TinfoilClient>>,
    ) -> Self {
        let (allowlist, allowlist_path) = Self::load_allowlist_best_effort();
        let approval_store = Arc::new(std::sync::Mutex::new(ApprovalStore::default()));
        let typed_action_executor = Arc::new(std::sync::Mutex::new(DaemonActionExecutor::new(
            approval_store.clone(),
            crate::approval::DEFAULT_APPROVAL_CAPACITY,
        )));
        Self {
            bridge,
            execution_mode,
            signing_key,
            auth: Arc::new(std::sync::Mutex::new(AuthState {
                allowlist,
                allowlist_path,
                expected_pin: None,
            })),
            audit,
            current_ticket,
            clarify,
            tinfoil_client,
            approval_store,
            typed_action_executor,
        }
    }

    /// Creates a new control channel with auth enforced. Any device not
    /// already in the persisted allowlist must supply `expected_pin`.
    /// `expected_pin` is typically [`crate::allowlist::generate_default_pin`]'s
    /// output, displayed to the user alongside the ticket/QR at startup.
    /// Devices that pass the PIN check are added to the allowlist and
    /// persisted immediately, so they don't need the PIN again on the
    /// next connection.
    ///
    /// This is the constructor `PAIRING.md` designs `main.rs` around, but
    /// `main.rs` does not call it yet. See that file's "Exact remaining
    /// wiring step" section.
    pub fn with_auth(
        bridge: Arc<HoloBridge>,
        execution_mode: ExecutionMode,
        signing_key: Arc<iroh::SecretKey>,
        expected_pin: String,
        audit: Arc<AuditLogger>,
        current_ticket: Arc<str>,
        clarify: Option<crate::clarify::ClarifyConfig>,
        tinfoil_client: Option<Arc<crate::tinfoil_client::TinfoilClient>>,
    ) -> Self {
        let (allowlist, allowlist_path) = Self::load_allowlist_best_effort();
        let approval_store = Arc::new(std::sync::Mutex::new(ApprovalStore::default()));
        let typed_action_executor = Arc::new(std::sync::Mutex::new(DaemonActionExecutor::new(
            approval_store.clone(),
            crate::approval::DEFAULT_APPROVAL_CAPACITY,
        )));
        Self {
            bridge,
            execution_mode,
            signing_key,
            auth: Arc::new(std::sync::Mutex::new(AuthState {
                allowlist,
                allowlist_path,
                expected_pin: Some(expected_pin),
            })),
            audit,
            current_ticket,
            clarify,
            tinfoil_client,
            approval_store,
            typed_action_executor,
        }
    }

    pub fn with_action_executor(
        mut self,
        typed_action_executor: Arc<std::sync::Mutex<DaemonActionExecutor>>,
    ) -> Self {
        self.approval_store = typed_action_executor
            .lock()
            .expect("typed action executor lock poisoned")
            .approval_store();
        self.typed_action_executor = typed_action_executor;
        self
    }

    pub fn typed_action_executor(&self) -> Arc<std::sync::Mutex<DaemonActionExecutor>> {
        self.typed_action_executor.clone()
    }

    pub fn approval_store(&self) -> Arc<std::sync::Mutex<ApprovalStore>> {
        self.approval_store.clone()
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
                (
                    Allowlist::default(),
                    std::path::PathBuf::from(".holoiroh-allowlist-fallback.json"),
                )
            }
        }
    }

    /// Runs the auth gate for a newly-accepted `connection`'s peer.
    ///
    /// Returns `Ok(())` if the connection may proceed. This happens when
    /// the device was already allowlisted, or when auth is disabled via
    /// [`Self::new`]. Returns `Err(reason)` if the connection must be
    /// rejected; `reason` is meant to be sent back as a
    /// [`ServerMessage::auth_rejected`] before closing.
    ///
    /// For an unknown device with PIN auth enabled, this reads exactly one
    /// bounded NDJSON frame from `reader`, expecting `{"type":"pin","pin":"..."}`. This must
    /// be the very first line the peer sends before anything else is
    /// processed. A `Prompt`/`VoiceTranscript`/`Stop` sent before a
    /// successful `Pin` from an unknown device is rejected, not queued or
    /// buffered. On a correct PIN, the device id is persisted to the
    /// allowlist immediately via [`Allowlist::save`], so future
    /// connections skip the PIN step.
    ///
    /// This function takes `auth` explicitly, rather than being a `&self`
    /// method reaching for `self.auth`, so it can be exercised directly.
    /// `examples/auth_gate_probe.rs` (`cargo run --example
    /// auth_gate_probe`) exercises it live, without needing a real
    /// `Arc<HoloBridge>` (which requires a live `holo serve` subprocess)
    /// to construct a full `ControlChannel`. [`ControlChannel::accept`]
    /// simply calls `authenticate(&self.auth, ...)`. This function is
    /// `pub`, not private, so that probe -- a real run-by-hand live
    /// witness for this gate, per this repo's no-unit-tests rule -- can
    /// call the actual function.
    pub async fn authenticate<R>(
        auth: &Arc<std::sync::Mutex<AuthState>>,
        remote: &str,
        reader: &mut R,
    ) -> std::result::Result<(), String>
    where
        R: tokio::io::AsyncBufRead + Unpin,
    {
        let remote_log = short_device_id(remote);
        if !is_valid_device_id(remote) {
            warn!(
                peer = remote_log,
                "control channel: invalid authenticated endpoint id rejected"
            );
            return Err(
                "authenticated endpoint id is not a full lowercase 64-hex value".to_string(),
            );
        }
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
        let line = match read_bounded_ndjson_line(reader, MAX_AUTH_FRAME_BYTES).await {
            Ok(Some(line)) => line,
            Ok(None) => return Err("connection closed before PIN was presented".to_string()),
            Err(FrameReadError::TooLarge {
                limit,
                frame_digest,
            }) => {
                warn!(
                    peer = remote_log,
                    message_kind = "unknown",
                    message_id = UNAVAILABLE_LOG_IDENTIFIER,
                    request_id = UNAVAILABLE_LOG_IDENTIFIER,
                    byte_count_at_least = limit + 1,
                    frame_digest,
                    digest_scope = "bounded_prefix",
                    "control channel: oversized pre-auth frame"
                );
                return Err(format!("PIN frame exceeds {limit}-byte limit"));
            }
            Err(FrameReadError::InvalidUtf8 {
                byte_count,
                frame_digest,
            }) => {
                warn!(
                    peer = remote_log,
                    message_kind = "unknown",
                    message_id = UNAVAILABLE_LOG_IDENTIFIER,
                    request_id = UNAVAILABLE_LOG_IDENTIFIER,
                    byte_count,
                    frame_digest,
                    parse_error = "invalid utf-8",
                    "control channel: malformed pre-auth frame"
                );
                return Err("PIN frame is not valid UTF-8".to_string());
            }
            Err(FrameReadError::Io(error)) => {
                warn!(
                    peer = remote_log,
                    error_kind = ?error.kind(),
                    "control channel: read error waiting for PIN"
                );
                return Err(format!("read error waiting for PIN: {}", error.kind()));
            }
        };
        let byte_count = line.len();
        let frame_digest = control_frame_digest(line.as_bytes());

        let value: serde_json::Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(error) => {
                let parse_error = safe_json_error(&error);
                warn!(
                    peer = remote_log,
                    message_kind = "unknown",
                    message_id = UNAVAILABLE_LOG_IDENTIFIER,
                    request_id = UNAVAILABLE_LOG_IDENTIFIER,
                    byte_count,
                    frame_digest,
                    parse_error,
                    "control channel: malformed pre-auth JSON"
                );
                return Err(format!("expected a PIN message first, got {parse_error}"));
            }
        };
        let message_kind = known_client_message_kind(&value);
        let message_id = envelope_message_id(&value);
        let request_id = payload_request_id(&value);
        let msg: ClientMessage = match serde_json::from_value(value) {
            Ok(msg) => msg,
            Err(error) => {
                let parse_error = safe_json_error(&error);
                warn!(
                    peer = remote_log,
                    message_kind,
                    message_id,
                    request_id,
                    byte_count,
                    frame_digest,
                    parse_error,
                    "control channel: malformed pre-auth message"
                );
                return Err(format!("expected a PIN message first, got {parse_error}"));
            }
        };

        let candidate = match msg {
            ClientMessage::Pin { pin } => pin,
            other => {
                warn!(
                    peer = remote_log,
                    message_kind = other.type_tag(),
                    message_id,
                    request_id,
                    byte_count,
                    "control channel: non-PIN pre-auth message rejected"
                );
                return Err(format!(
                    "expected a PIN message first from an unrecognized device, got {} instead",
                    other.type_tag()
                ));
            }
        };

        let mut state = auth.lock().expect("auth lock poisoned");
        let expected = state.expected_pin.clone().expect(
            "checked Some above; not mutated between the two locks on this task-local path",
        );

        if !verify_pin(&candidate, &expected) {
            warn!(
                peer = remote_log,
                message_kind = "pin",
                message_id,
                request_id,
                byte_count,
                "control channel: incorrect PIN rejected"
            );
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
            warn!(peer = %remote_log, error = %err, "control channel: PIN accepted but failed to persist allowlist -- device will need to re-pair after daemon restart");
        }
        info!(peer = %remote_log, "control channel: new device paired via PIN, added to allowlist");
        Ok(())
    }

    /// Mounts this control channel's [`ProtocolHandler`] onto `router`
    /// under [`CONTROL_ALPN`], alongside whatever other protocols are
    /// already registered on the same `Endpoint` (for example,
    /// `iroh-live`'s MoQ/gossip via `Live::register_protocols`). This
    /// function mirrors `iroh_live::Live::register_protocols`'s own
    /// signature, so the two compose in `main.rs` with the same
    /// builder-chaining pattern:
    ///
    /// ```ignore
    /// let router = live.register_protocols(RouterBuilder::new(endpoint));
    /// let router = control.register_protocols(router);
    /// let router = router.spawn();
    /// ```
    pub fn register_protocols(&self, router: RouterBuilder) -> RouterBuilder {
        router.accept(CONTROL_ALPN, self.clone())
    }
}

impl ProtocolHandler for ControlChannel {
    async fn accept(&self, connection: Connection) -> std::result::Result<(), AcceptError> {
        let remote_id = connection.remote_id();
        let remote_device_id = remote_id.to_string();
        let remote = remote_id.fmt_short();
        info!(peer = %remote, "control channel: accepted connection");

        let (mut send, recv) = connection
            .accept_bi()
            .await
            .map_err(AcceptError::from_err)?;

        let mut reader = BufReader::new(recv);

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
        if let Err(reason) = Self::authenticate(&self.auth, &remote_device_id, &mut reader).await {
            warn!(peer = %remote, "control channel: rejecting connection, auth failed");
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
        let outbound_state = OutboundEnvelopeState::new();
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
            signing_key: &iroh::SecretKey,
            recipient: &iroh::PublicKey,
            session_id: &str,
            task_id: Option<String>,
            msg: ServerMessage,
        ) -> Result<()>
        where
            W: tokio::io::AsyncWrite + Unpin,
        {
            let seq = outbound_state.next_outbound_sequence();
            let mut envelope =
                TaskEnvelope::<ServerMessage>::wrap(session_id.to_string(), task_id, seq, msg);
            sign_daemon_envelope(&mut envelope, signing_key, recipient)?;
            write_line(send, &envelope).await?;
            Ok(())
        }

        // Metadata-only audit log (Project Aro PRD row P0-12, see `crate::audit_log`'s module
        // doc): the connection's direct-vs-relay path is determined once, here, from the live
        // `Connection` -- it cannot change for the lifetime of this accepted connection (a new
        // path renegotiation would be a new `Connection`), so every task audited on this
        // connection shares one `ConnectionPath` value rather than re-deriving it per task.
        let connection_path = ConnectionPath::from_connection(&connection);
        let mut initial_messages: Vec<(Option<String>, ServerMessage)> = vec![
            (
                None,
                ServerMessage::greeting(
                    "control channel ready",
                    self.execution_mode.wire_name(),
                    self.execution_mode.capabilities().iter().copied(),
                ),
            ),
            (None, ServerMessage::current_ticket(&*self.current_ticket)),
        ];

        if let Some(client) = &self.tinfoil_client {
            match serde_json::from_str(client.ground_truth_json().as_ref()) {
                Ok(ground_truth) => {
                    let message = ServerMessage::TinfoilVerification {
                        host: client.base_url(),
                        ground_truth,
                    };
                    initial_messages.push((None, message));
                }
                Err(err) => {
                    warn!(error = %err, "control channel: verified Tinfoil ground truth was not JSON");
                }
            }
        }

        // Reconnect visibility: if a Holo task survived a previous connection's drop (still
        // running, or prompts still queued behind it -- see `HoloControlBridge::busy_state`),
        // tell the newly (re)connected peer immediately rather than leaving it to guess from
        // silence until the next `ControlEvent` happens to arrive. This is the direct fix for
        // "a stale in-flight Holo task should not be silently abandoned -- surface its
        // last-known state on reconnect".
        let (busy, queued) = self.bridge.busy_state();
        // A PARKED (paused) task is not `busy` -- pausing cancels the backend turn
        // and keeps it in the bridge's `paused` slot -- so without this check a
        // paused task from before the drop would trigger NO reconnect notice at
        // all, leaving the phone with no way to resume or stop it.
        let paused = self.bridge.is_paused();
        if busy || paused || queued > 0 {
            let text = match (busy, paused, queued) {
                (true, _, 0) => "reconnected: a Holo task is still running from before".to_string(),
                (true, _, n) => format!(
                    "reconnected: a Holo task is still running from before, {n} more queued behind it"
                ),
                (false, true, 0) => {
                    "reconnected: a Holo task is paused from before -- resume or stop it".to_string()
                }
                (false, true, n) => format!(
                    "reconnected: a Holo task is paused from before, {n} more queued behind it"
                ),
                (false, false, n) => format!("reconnected: {n} queued Holo task(s) waiting to run"),
            };
            initial_messages.push((None, ServerMessage::status(text)));
            initial_messages.push((
                None,
                ServerMessage::TaskActive {
                    paused: paused && !busy,
                    queued,
                },
            ));
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
        let tinfoil_operations = Arc::new(Semaphore::new(MAX_TINFOIL_OPERATIONS_PER_CONNECTION));
        let mut tinfoil_tasks = JoinSet::new();
        let typed_continuations: TypedContinuationRegistry =
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        let active_typed_tasks: ActiveTypedTaskRegistry =
            Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));

        // Point the bridge at THIS connection now, on accept -- not later, on the
        // first dispatchable message.
        //
        // The only other `replace_event_sink` call lives inside the read loop's
        // payload-dispatch arm, and `Pin`/`InputResponse`/`ClarifyRequest` all
        // `continue` before reaching it. A reconnecting phone sends a bare PIN
        // first, so it was greeted with "a Holo task is still running from
        // before" and a TaskActive pill, and then every subsequent `emit` went to
        // the PREVIOUS connection's dead channel: progress lines, the terminal
        // `task_done`, the sensitive-app consent `InputRequest`, and
        // `SecureInputState`. The pill hung in Working forever and a consent gate
        // could expire without ever being shown. `emit` deliberately swallows the
        // send error, so nothing logged it either.
        //
        // Doing it here also closes the cold-start window in which `main.rs`'s
        // long-lived `_bridge_events_rx` binding silently absorbed every event
        // emitted before the first phone connected. Auth has already passed at
        // this point, and the call at the dispatch site below stays as an
        // idempotent re-assert.
        self.bridge.replace_event_sink(events_tx.clone());

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
        let typed_action_executor = self.typed_action_executor.clone();

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
            let remote = remote.to_string();
            let session_id = session_id.clone();
            let current_task_id = current_task_id.clone();
            let audit = self.audit.clone();
            let audit_starts = audit_starts.clone();
            let typed_action_executor = typed_action_executor.clone();
            let typed_continuations = typed_continuations.clone();
            let active_typed_tasks = active_typed_tasks.clone();
            let signing_key = self.signing_key.clone();
            let recipient = remote_id;
            async move {
                let mut outbound_state = outbound_state;
                for (task_id, message) in initial_messages {
                    if let Err(error) = send_envelope(
                        &mut send,
                        &mut outbound_state,
                        &signing_key,
                        &recipient,
                        &session_id,
                        task_id,
                        message,
                    )
                    .await
                    {
                        warn!(peer = %remote, error = %error, "control channel: failed to write initial signed envelope");
                        return;
                    }
                }
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
                    if let ControlEvent::Done { request_id, .. } = &event {
                        cancel_typed_continuations(
                            &typed_continuations,
                            &active_typed_tasks,
                            &session_id,
                            Some(request_id),
                        )
                        .await;
                        typed_action_executor
                            .lock()
                            .expect("typed_action_executor lock poisoned")
                            .cancel_task_id(request_id);
                    }
                    // Correlate by the event's OWN request_id first (concurrent turns are
                    // real now that the read loop spawns them); the last-inbound-envelope
                    // fallback only covers events that genuinely carry no id of their own.
                    let task_id = if matches!(
                        &event,
                        ControlEvent::Error { request_id, .. } if request_id.is_empty()
                    ) {
                        None
                    } else {
                        event_request_id(&event).or_else(|| {
                            current_task_id
                                .lock()
                                .expect("current_task_id lock poisoned")
                                .clone()
                        })
                    };
                    let msg = from_control_event(event);
                    if let Err(err) = send_envelope(
                        &mut send,
                        &mut outbound_state,
                        &signing_key,
                        &recipient,
                        &session_id,
                        task_id,
                        msg,
                    )
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
        let mut inbound_state = InboundEnvelopeState::for_session(session_id.clone());
        let mut first_inbound_envelope = true;

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
                line = read_bounded_ndjson_line(&mut reader, MAX_CONTROL_FRAME_BYTES) => line,
                completed = tinfoil_tasks.join_next(), if !tinfoil_tasks.is_empty() => {
                    if let Some(Err(error)) = completed {
                        warn!(
                            peer = %remote,
                            cancelled = error.is_cancelled(),
                            panicked = error.is_panic(),
                            "control channel: Tinfoil operation task ended unexpectedly"
                        );
                    }
                    continue;
                }
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
                Err(FrameReadError::TooLarge {
                    limit,
                    frame_digest,
                }) => {
                    warn!(
                        peer = %remote,
                        message_kind = "unknown",
                        message_id = UNAVAILABLE_LOG_IDENTIFIER,
                        request_id = UNAVAILABLE_LOG_IDENTIFIER,
                        byte_count_at_least = limit + 1,
                        frame_digest,
                        digest_scope = "bounded_prefix",
                        "control channel: oversized authenticated frame rejected"
                    );
                    let _ = events_tx.send(ControlEvent::Error {
                        request_id: String::new(),
                        message: format!("control frame exceeds {limit}-byte limit"),
                    });
                    break;
                }
                Err(FrameReadError::InvalidUtf8 {
                    byte_count,
                    frame_digest,
                }) => {
                    warn!(
                        peer = %remote,
                        message_kind = "unknown",
                        message_id = UNAVAILABLE_LOG_IDENTIFIER,
                        request_id = UNAVAILABLE_LOG_IDENTIFIER,
                        byte_count,
                        frame_digest,
                        parse_error = "invalid utf-8",
                        "control channel: malformed authenticated frame"
                    );
                    break;
                }
                Err(FrameReadError::Io(error)) => {
                    warn!(
                        peer = %remote,
                        error_kind = ?error.kind(),
                        "control channel: read error"
                    );
                    break;
                }
            };
            let frame_byte_count = line.len();
            let frame_digest = control_frame_digest(line.as_bytes());

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
                    let safe_error = safe_json_error(&parse_err);
                    warn!(
                        peer = %remote,
                        message_kind = "unknown",
                        message_id = UNAVAILABLE_LOG_IDENTIFIER,
                        request_id = UNAVAILABLE_LOG_IDENTIFIER,
                        byte_count = frame_byte_count,
                        frame_digest,
                        parse_error = safe_error,
                        "control channel: malformed envelope JSON"
                    );
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

            let envelope_message_kind = known_client_message_kind(&envelope_value);
            let log_message_id = envelope_message_id(&envelope_value);
            let log_request_id = payload_request_id(&envelope_value);
            let envelope: TaskEnvelope<serde_json::Value> = match serde_json::from_value(
                envelope_value,
            ) {
                Ok(env) => env,
                Err(shape_err) => {
                    let safe_error = safe_json_error(&shape_err);
                    warn!(
                        peer = %remote,
                        message_kind = envelope_message_kind,
                        message_id = log_message_id,
                        request_id = log_request_id,
                        byte_count = frame_byte_count,
                        frame_digest,
                        parse_error = safe_error,
                        "control channel: envelope missing required framing fields"
                    );
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

            // Authenticate the complete envelope shell before payload deserialization
            // or any session/replay/correlation state mutation.
            let local_public = self.signing_key.public();
            if let Err(reason) = verify_client_envelope(&envelope, &remote_id, &local_public) {
                warn!(
                    peer = %remote,
                    message_kind = envelope_message_kind,
                    message_id = log_message_id,
                    request_id = log_request_id,
                    byte_count = frame_byte_count,
                    frame_digest,
                    rejection = %reason,
                    "control channel: envelope signature rejected"
                );
                if events_tx
                    .send(ControlEvent::Error {
                        request_id: String::new(),
                        message: "envelope rejected".to_string(),
                    })
                    .is_err()
                {
                    break;
                }
                continue;
            }

            let payload_message_kind = known_client_message_kind(&envelope.payload);
            let msg = match serde_json::from_value::<ClientMessage>(envelope.payload.clone()) {
                Ok(message) => message,
                Err(parse_err) => {
                    let safe_error = safe_json_error(&parse_err);
                    warn!(
                        peer = %remote,
                        message_kind = payload_message_kind,
                        message_id = log_message_id,
                        request_id = log_request_id,
                        byte_count = frame_byte_count,
                        frame_digest,
                        parse_error = safe_error,
                        "control channel: malformed payload"
                    );
                    if events_tx
                        .send(ControlEvent::Error {
                            request_id: String::new(),
                            message: "malformed payload".to_string(),
                        })
                        .is_err()
                    {
                        break;
                    }
                    continue;
                }
            };

            if let Err(rejection) = admit_post_signature_envelope(
                &envelope,
                &msg,
                &session_id,
                first_inbound_envelope,
                &mut inbound_state,
                self.execution_mode,
                &typed_action_executor,
                epoch_millis_now(),
            ) {
                warn!(
                    peer = %remote,
                    message_kind = payload_message_kind,
                    message_id = log_message_id,
                    request_id = log_request_id,
                    byte_count = frame_byte_count,
                    frame_digest,
                    rejection,
                    "control channel: post-signature admission rejected"
                );
                if events_tx
                    .send(ControlEvent::Error {
                        request_id: String::new(),
                        message: "envelope rejected".to_string(),
                    })
                    .is_err()
                {
                    break;
                }
                continue;
            }
            first_inbound_envelope = false;

            *current_task_id
                .lock()
                .expect("current_task_id lock poisoned") = envelope.task_id.clone();

            match Ok::<ClientMessage, serde_json::Error>(msg) {
                Ok(ClientMessage::Pin { .. }) => {
                    // A Pin sent after auth already passed (e.g. an already-
                    // allowlisted device, or a second Pin from a device that
                    // just paired) is redundant, not an error -- ack it and
                    // keep reading rather than tearing down the connection.
                    debug!(peer = %remote, "control channel: redundant Pin message after auth, acking");
                    if events_tx
                        .send(ControlEvent::Ack {
                            request_id: String::new(),
                        })
                        .is_err()
                    {
                        break;
                    }
                }
                Ok(ClientMessage::ApprovalResponse { response }) => {
                    let approval_id = response.approval_id.clone();
                    let continuation = typed_continuations
                        .lock()
                        .expect("typed continuation registry lock poisoned")
                        .remove(&approval_id);
                    if let Some(continuation) = continuation {
                        let tx = events_tx.clone();
                        let continuations = typed_continuations.clone();
                        let active_tasks = active_typed_tasks.clone();
                        tinfoil_tasks.spawn(async move {
                            let result = continuation
                                .agent_loop
                                .lock()
                                .await
                                .resume_approved(&approval_id)
                                .await;
                            publish_typed_loop_result(
                                result,
                                continuation.session_id,
                                continuation.task_id,
                                continuation.agent_loop,
                                continuation.permit,
                                continuations,
                                active_tasks,
                                tx,
                            )
                            .await;
                        });
                    } else if events_tx
                        .send(ControlEvent::Ack {
                            request_id: response.approval_id,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
                Ok(ClientMessage::TypedPrompt { prompt }) => {
                    let request_id = envelope
                        .task_id
                        .clone()
                        .expect("typed prompt task binding admitted");
                    let tx = events_tx.clone();
                    let executor = typed_action_executor.clone();
                    match self.tinfoil_client.clone() {
                        Some(client) => {
                            let permit = match tinfoil_operations.clone().try_acquire_owned() {
                                Ok(permit) => permit,
                                Err(_) => {
                                    if tx
                                        .send(ControlEvent::Error {
                                            request_id,
                                            message: TINFOIL_BUSY_ERROR.to_string(),
                                        })
                                        .is_err()
                                    {
                                        break;
                                    }
                                    continue;
                                }
                            };
                            let bindings = match TrustedTaskBindings::new(
                                &prompt.goal_id,
                                &prompt.instruction,
                                &session_id,
                                &envelope.message_id,
                                &request_id,
                            ) {
                                Ok(bindings) => bindings,
                                Err(_) => {
                                    if tx
                                        .send(ControlEvent::Error {
                                            request_id,
                                            message: "invalid typed prompt bindings".to_string(),
                                        })
                                        .is_err()
                                    {
                                        break;
                                    }
                                    continue;
                                }
                            };
                            let audit = self.audit.clone();
                            let continuations = typed_continuations.clone();
                            let active_tasks = active_typed_tasks.clone();
                            let loop_session_id = session_id.clone();
                            let planner = TinfoilTurnPlanner::new(client);
                            let loop_value = build_production_typed_loop(
                                executor,
                                planner,
                                AgentLoopLimits::default(),
                            );
                            let canceled = loop_value.cancellation_handle();
                            let execution_gate = loop_value.execution_gate();
                            let agent_loop = Arc::new(tokio::sync::Mutex::new(loop_value));
                            {
                                let mut active = active_typed_tasks.lock().await;
                                if active.contains_key(&request_id) {
                                    let _ = tx.send(ControlEvent::Error {
                                        request_id,
                                        message: "typed task binding is already active".to_string(),
                                    });
                                    continue;
                                }
                                active.insert(
                                    request_id.clone(),
                                    (loop_session_id.clone(), canceled, execution_gate),
                                );
                            }
                            tinfoil_tasks.spawn(async move {
                                let _ = tx.send(ControlEvent::Ack {
                                    request_id: request_id.clone(),
                                });
                                let byte_count = prompt.instruction.len() as u64;
                                let result = agent_loop.lock().await.run_bound(bindings).await;
                                let success = matches!(
                                    &result,
                                    Ok(AgentLoopOutcome::Completed { .. })
                                );
                                log_cloud_egress(
                                    &audit,
                                    &request_id,
                                    crate::audit_log::CloudEgressCapability::Planner,
                                    success,
                                    byte_count,
                                );
                                publish_typed_loop_result(
                                    result,
                                    loop_session_id,
                                    request_id,
                                    agent_loop,
                                    permit,
                                    continuations,
                                    active_tasks,
                                    tx,
                                )
                                .await;
                            });
                        }
                        None => {
                            if tx
                                .send(ControlEvent::Error {
                                    request_id,
                                    message: "no attested Tinfoil planner configured".to_string(),
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                }
                Ok(ClientMessage::InputResponse {
                    request_id,
                    selected_option,
                }) => {
                    // Sensitive-app consent decisions are resolved by the bridge itself
                    // (it owns the paused turn + allowance state); everything else falls
                    // through to this connection's generic pending-input tracking.
                    if HoloControlBridge::resolve_consent(
                        &self.bridge,
                        &request_id,
                        &selected_option,
                    ) {
                        debug!(
                            peer = %remote,
                            message_kind = "input_response",
                            byte_count = frame_byte_count,
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
                                message_kind = "input_response",
                                byte_count = frame_byte_count,
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
                                message_kind = "input_response",
                                byte_count = frame_byte_count,
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
                Ok(ClientMessage::ClarifyRequest { prompt }) => {
                    // Clarification runs OFF the desktop-task pipeline: spawn the
                    // (up to ~20s) inference so the read loop keeps draining, and
                    // deliver the result as a ControlEvent the writer turns into
                    // ServerMessage::ClarifyQuestions. No clarify config (no
                    // TINFOIL_API_KEY) replies immediately with an empty set so the
                    // app proceeds with a direct send.
                    let clarify_tx = events_tx.clone();
                    match self.clarify.clone() {
                        Some(config) => {
                            let permit = match tinfoil_operations.clone().try_acquire_owned() {
                                Ok(permit) => permit,
                                Err(_) => {
                                    warn!(
                                        peer = %remote,
                                        message_kind = "clarify_request",
                                        limit = MAX_TINFOIL_OPERATIONS_PER_CONNECTION,
                                        "control channel: Tinfoil operation limit reached"
                                    );
                                    if clarify_tx
                                        .send(ControlEvent::ClarifyQuestions {
                                            questions: Vec::new(),
                                        })
                                        .is_err()
                                    {
                                        break;
                                    }
                                    continue;
                                }
                            };
                            tinfoil_tasks.spawn(async move {
                                let _permit = permit;
                                let questions =
                                    crate::clarify::generate_clarifying_questions(&prompt, &config)
                                        .await;
                                let _ =
                                    clarify_tx.send(ControlEvent::ClarifyQuestions { questions });
                            });
                        }
                        None => {
                            if clarify_tx
                                .send(ControlEvent::ClarifyQuestions {
                                    questions: Vec::new(),
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                }
                Ok(ClientMessage::ProcessDocument {
                    request_id,
                    filename,
                    data_base64,
                    mode,
                }) => {
                    // Off the desktop-task pipeline, same shape as ClarifyRequest: spawn the
                    // (potentially slow, up to 120s per tinfoil_documents) call so the read loop
                    // keeps draining, deliver the result as a ControlEvent.
                    let tx = events_tx.clone();
                    let audit = self.audit.clone();
                    match self.tinfoil_client.clone() {
                        Some(client) => {
                            let permit = match tinfoil_operations.clone().try_acquire_owned() {
                                Ok(permit) => permit,
                                Err(_) => {
                                    log_cloud_egress(
                                        &audit,
                                        &request_id,
                                        crate::audit_log::CloudEgressCapability::Document,
                                        false,
                                        0,
                                    );
                                    if tx
                                        .send(ControlEvent::DocumentProcessFailed {
                                            request_id,
                                            error: TINFOIL_BUSY_ERROR.to_string(),
                                        })
                                        .is_err()
                                    {
                                        break;
                                    }
                                    continue;
                                }
                            };
                            tinfoil_tasks.spawn(async move {
                                let _permit = permit;
                                let bytes = match base64::Engine::decode(
                                    &base64::engine::general_purpose::STANDARD,
                                    data_base64,
                                ) {
                                    Ok(b) => b,
                                    Err(err) => {
                                        log_cloud_egress(
                                            &audit,
                                            &request_id,
                                            crate::audit_log::CloudEgressCapability::Document,
                                            false,
                                            0,
                                        );
                                        let _ = tx.send(ControlEvent::DocumentProcessFailed {
                                            request_id,
                                            error: format!("invalid base64: {err}"),
                                        });
                                        return;
                                    }
                                };
                                let byte_count = bytes.len() as u64;
                                let convert_mode = match mode.as_str() {
                                    "vision" => crate::tinfoil_documents::ConvertMode::Vision,
                                    "images" => crate::tinfoil_documents::ConvertMode::Images,
                                    "raw" => crate::tinfoil_documents::ConvertMode::Raw,
                                    "vlm" => crate::tinfoil_documents::ConvertMode::Vlm,
                                    _ => crate::tinfoil_documents::ConvertMode::Text,
                                };
                                let files = vec![crate::tinfoil_documents::DocumentInput {
                                    filename,
                                    bytes,
                                }];
                                let audit_request_id = request_id.clone();
                                let (event, success) =
                                    match crate::tinfoil_documents::convert_documents(
                                        &client,
                                        &files,
                                        convert_mode,
                                    )
                                    .await
                                    {
                                        Ok(docs) => {
                                            let markdown = docs
                                                .into_iter()
                                                .map(|d| d.markdown)
                                                .collect::<Vec<_>>()
                                                .join("\n\n---\n\n");
                                            (
                                                ControlEvent::DocumentProcessed {
                                                    request_id,
                                                    markdown,
                                                },
                                                true,
                                            )
                                        }
                                        Err(err) => (
                                            ControlEvent::DocumentProcessFailed {
                                                request_id,
                                                error: err.to_string(),
                                            },
                                            false,
                                        ),
                                    };
                                log_cloud_egress(
                                    &audit,
                                    &audit_request_id,
                                    crate::audit_log::CloudEgressCapability::Document,
                                    success,
                                    byte_count,
                                );
                                let _ = tx.send(event);
                            });
                        }
                        None => {
                            if tx
                                .send(ControlEvent::DocumentProcessFailed {
                                    request_id,
                                    error: "no TINFOIL_API_KEY configured".to_string(),
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                }
                Ok(ClientMessage::AnalyzeImage {
                    request_id,
                    image_data_base64,
                    prompt,
                }) => {
                    let tx = events_tx.clone();
                    let audit = self.audit.clone();
                    match self.tinfoil_client.clone() {
                        Some(client) => {
                            let permit = match tinfoil_operations.clone().try_acquire_owned() {
                                Ok(permit) => permit,
                                Err(_) => {
                                    log_cloud_egress(
                                        &audit,
                                        &request_id,
                                        crate::audit_log::CloudEgressCapability::Image,
                                        false,
                                        0,
                                    );
                                    if tx
                                        .send(ControlEvent::ImageAnalysisFailed {
                                            request_id,
                                            error: TINFOIL_BUSY_ERROR.to_string(),
                                        })
                                        .is_err()
                                    {
                                        break;
                                    }
                                    continue;
                                }
                            };
                            tinfoil_tasks.spawn(async move {
                                let _permit = permit;
                                let bytes = match base64::Engine::decode(
                                    &base64::engine::general_purpose::STANDARD,
                                    image_data_base64,
                                ) {
                                    Ok(b) => b,
                                    Err(err) => {
                                        log_cloud_egress(
                                            &audit,
                                            &request_id,
                                            crate::audit_log::CloudEgressCapability::Image,
                                            false,
                                            0,
                                        );
                                        let _ = tx.send(ControlEvent::ImageAnalysisFailed {
                                            request_id,
                                            error: format!("invalid base64: {err}"),
                                        });
                                        return;
                                    }
                                };
                                let byte_count = bytes.len() as u64;
                                let image = match image::load_from_memory(&bytes) {
                                    Ok(img) => img,
                                    Err(err) => {
                                        log_cloud_egress(
                                            &audit,
                                            &request_id,
                                            crate::audit_log::CloudEgressCapability::Image,
                                            false,
                                            byte_count,
                                        );
                                        let _ = tx.send(ControlEvent::ImageAnalysisFailed {
                                            request_id,
                                            error: format!("failed to decode image: {err}"),
                                        });
                                        return;
                                    }
                                };
                                let audit_request_id = request_id.clone();
                                let (event, success) = match crate::tinfoil_vision::analyze_image(
                                    &client,
                                    &image,
                                    &prompt,
                                    crate::tinfoil_vision::VisionModel::Gemma431b,
                                )
                                .await
                                {
                                    Ok(text) => {
                                        (ControlEvent::ImageAnalyzed { request_id, text }, true)
                                    }
                                    Err(err) => (
                                        ControlEvent::ImageAnalysisFailed {
                                            request_id,
                                            error: err.to_string(),
                                        },
                                        false,
                                    ),
                                };
                                log_cloud_egress(
                                    &audit,
                                    &audit_request_id,
                                    crate::audit_log::CloudEgressCapability::Image,
                                    success,
                                    byte_count,
                                );
                                let _ = tx.send(event);
                            });
                        }
                        None => {
                            if tx
                                .send(ControlEvent::ImageAnalysisFailed {
                                    request_id,
                                    error: "no TINFOIL_API_KEY configured".to_string(),
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                }
                Ok(ClientMessage::TranscribeAudio {
                    request_id,
                    audio_data_base64,
                    format,
                }) => {
                    let tx = events_tx.clone();
                    let audit = self.audit.clone();
                    let filename = match transcription_filename_for_format(&format) {
                        Ok(filename) => filename,
                        Err(error) => {
                            log_cloud_egress(
                                &audit,
                                &request_id,
                                crate::audit_log::CloudEgressCapability::AudioTranscribe,
                                false,
                                0,
                            );
                            if tx
                                .send(ControlEvent::AudioTranscriptionFailed {
                                    request_id,
                                    error: error.to_string(),
                                })
                                .is_err()
                            {
                                break;
                            }
                            continue;
                        }
                    };
                    match self.tinfoil_client.clone() {
                        Some(client) => {
                            let permit = match tinfoil_operations.clone().try_acquire_owned() {
                                Ok(permit) => permit,
                                Err(_) => {
                                    log_cloud_egress(
                                        &audit,
                                        &request_id,
                                        crate::audit_log::CloudEgressCapability::AudioTranscribe,
                                        false,
                                        0,
                                    );
                                    if tx
                                        .send(ControlEvent::AudioTranscriptionFailed {
                                            request_id,
                                            error: TINFOIL_BUSY_ERROR.to_string(),
                                        })
                                        .is_err()
                                    {
                                        break;
                                    }
                                    continue;
                                }
                            };
                            tinfoil_tasks.spawn(async move {
                                let _permit = permit;
                                let bytes = match base64::Engine::decode(
                                    &base64::engine::general_purpose::STANDARD,
                                    audio_data_base64,
                                ) {
                                    Ok(b) => b,
                                    Err(err) => {
                                        log_cloud_egress(&audit, &request_id, crate::audit_log::CloudEgressCapability::AudioTranscribe, false, 0);
                                        let _ = tx.send(ControlEvent::AudioTranscriptionFailed {
                                            request_id,
                                            error: format!("invalid base64: {err}"),
                                        });
                                        return;
                                    }
                                };
                                let byte_count = bytes.len() as u64;
                                let audit_request_id = request_id.clone();
                                let (event, success) =
                                    match crate::tinfoil_audio::transcribe(
                                        &client, bytes, filename,
                                    )
                                        .await
                                    {
                                        Ok(text) => (
                                            ControlEvent::AudioTranscribed { request_id, text },
                                            true,
                                        ),
                                        Err(err) => (
                                            ControlEvent::AudioTranscriptionFailed {
                                                request_id,
                                                error: err.to_string(),
                                            },
                                            false,
                                        ),
                                    };
                                log_cloud_egress(
                                    &audit,
                                    &audit_request_id,
                                    crate::audit_log::CloudEgressCapability::AudioTranscribe,
                                    success,
                                    byte_count,
                                );
                                let _ = tx.send(event);
                            });
                        }
                        None => {
                            if tx
                                .send(ControlEvent::AudioTranscriptionFailed {
                                    request_id,
                                    error: "no TINFOIL_API_KEY configured".to_string(),
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                }
                Ok(ClientMessage::RequestSpeech {
                    request_id,
                    text,
                    voice,
                }) => {
                    let tx = events_tx.clone();
                    let audit = self.audit.clone();
                    match self.tinfoil_client.clone() {
                        Some(client) => {
                            let permit = match tinfoil_operations.clone().try_acquire_owned() {
                                Ok(permit) => permit,
                                Err(_) => {
                                    log_cloud_egress(
                                        &audit,
                                        &request_id,
                                        crate::audit_log::CloudEgressCapability::AudioSpeech,
                                        false,
                                        0,
                                    );
                                    if tx
                                        .send(ControlEvent::SpeechFailed {
                                            request_id,
                                            error: TINFOIL_BUSY_ERROR.to_string(),
                                        })
                                        .is_err()
                                    {
                                        break;
                                    }
                                    continue;
                                }
                            };
                            tinfoil_tasks.spawn(async move {
                                let _permit = permit;
                                let byte_count = text.len() as u64;
                                let audit_request_id = request_id.clone();
                                let (event, success) = match crate::tinfoil_audio::speech(
                                    &client, &text, &voice,
                                )
                                .await
                                {
                                    Ok(wav) => (
                                        ControlEvent::SpeechReady {
                                            request_id,
                                            audio_data_base64:
                                                crate::tinfoil_audio::encode_speech_base64(&wav),
                                        },
                                        true,
                                    ),
                                    Err(err) => (
                                        ControlEvent::SpeechFailed {
                                            request_id,
                                            error: err.to_string(),
                                        },
                                        false,
                                    ),
                                };
                                log_cloud_egress(
                                    &audit,
                                    &audit_request_id,
                                    crate::audit_log::CloudEgressCapability::AudioSpeech,
                                    success,
                                    byte_count,
                                );
                                let _ = tx.send(event);
                            });
                        }
                        None => {
                            if tx
                                .send(ControlEvent::SpeechFailed {
                                    request_id,
                                    error: "no TINFOIL_API_KEY configured".to_string(),
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                }
                Ok(ClientMessage::PlanTask { request_id, goal }) => {
                    let tx = events_tx.clone();
                    let audit = self.audit.clone();
                    match self.tinfoil_client.clone() {
                        Some(client) => {
                            let permit = match tinfoil_operations.clone().try_acquire_owned() {
                                Ok(permit) => permit,
                                Err(_) => {
                                    log_cloud_egress(
                                        &audit,
                                        &request_id,
                                        crate::audit_log::CloudEgressCapability::Planner,
                                        false,
                                        0,
                                    );
                                    if tx
                                        .send(ControlEvent::PlanFailed {
                                            request_id,
                                            error: TINFOIL_BUSY_ERROR.to_string(),
                                        })
                                        .is_err()
                                    {
                                        break;
                                    }
                                    continue;
                                }
                            };
                            tinfoil_tasks.spawn(async move {
                                let _permit = permit;
                                let byte_count = goal.len() as u64;
                                let audit_request_id = request_id.clone();
                                let (event, success) =
                                    match crate::tinfoil_planner::plan_task(&client, &goal).await {
                                        Ok(plan) => {
                                            let steps = plan
                                                .steps
                                                .iter()
                                                .map(|step| {
                                                    match step {
                                                crate::tinfoil_planner::PlannedStep::Action(
                                                    action,
                                                ) => {
                                                    format!("Typed action {:?}", action.action)
                                                }
                                                crate::tinfoil_planner::PlannedStep::Complete => {
                                                    "Complete".to_string()
                                                }
                                            }
                                                })
                                                .collect();
                                            (ControlEvent::PlanReady { request_id, steps }, true)
                                        }
                                        Err(err) => (
                                            ControlEvent::PlanFailed {
                                                request_id,
                                                error: err.to_string(),
                                            },
                                            false,
                                        ),
                                    };
                                log_cloud_egress(
                                    &audit,
                                    &audit_request_id,
                                    crate::audit_log::CloudEgressCapability::Planner,
                                    success,
                                    byte_count,
                                );
                                let _ = tx.send(event);
                            });
                        }
                        None => {
                            if tx
                                .send(ControlEvent::PlanFailed {
                                    request_id,
                                    error: "no TINFOIL_API_KEY configured".to_string(),
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                }
                Ok(msg) => {
                    if matches!(
                        &msg,
                        ClientMessage::Stop { .. }
                            | ClientMessage::Pause
                            | ClientMessage::Redirect { .. }
                    ) {
                        cancel_typed_continuations(
                            &typed_continuations,
                            &active_typed_tasks,
                            &session_id,
                            None,
                        )
                        .await;
                        typed_action_executor
                            .lock()
                            .expect("typed_action_executor lock poisoned")
                            .cancel_session(&session_id);
                    }
                    debug!(
                        peer = %remote,
                        message_kind = msg.type_tag(),
                        message_id = log_message_id,
                        request_id = log_request_id,
                        byte_count = frame_byte_count,
                        frame_digest,
                        "control channel: received message"
                    );
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
                        ClientMessage::Stop { .. }
                        | ClientMessage::Pause
                        | ClientMessage::Pin { .. }
                        | ClientMessage::InputResponse { .. }
                        | ClientMessage::ApprovalResponse { .. }
                        // Remote-control input events are not agent turns; they
                        // inject direct user input and produce no audit entry.
                        | ClientMessage::RemoteControl { .. }
                        // ClarifyRequest is handled by its own arm above (never
                        // reaches here); listed only to keep the match exhaustive.
                        | ClientMessage::ClarifyRequest { .. }
                        // Same: each has its own arm above, off the desktop-task pipeline, and
                        // is audited separately by control-channel-audit-logging (cloud egress),
                        // not as an agent-turn ActionClass.
                        | ClientMessage::TypedPrompt { .. }
                        | ClientMessage::ProcessDocument { .. }
                        | ClientMessage::AnalyzeImage { .. }
                        | ClientMessage::TranscribeAudio { .. }
                        | ClientMessage::RequestSpeech { .. }
                        | ClientMessage::PlanTask { .. } => None,
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
                    if must_preserve_arrival_order(&control_message) {
                        self.bridge.handle_message(control_message).await;
                        continue;
                    }
                    let bridge = self.bridge.clone();
                    tokio::spawn(async move {
                        bridge.handle_message(control_message).await;
                    });
                }
                Err(parse_err) => {
                    let safe_error = safe_json_error(&parse_err);
                    warn!(
                        peer = %remote,
                        message_kind = payload_message_kind,
                        message_id = log_message_id,
                        request_id = log_request_id,
                        byte_count = frame_byte_count,
                        frame_digest,
                        parse_error = safe_error,
                        "control channel: malformed payload"
                    );
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

        cancel_typed_continuations(
                            &typed_continuations,
                            &active_typed_tasks,
                            &session_id,
                            None,
                        )
                        .await;
        typed_action_executor
            .lock()
            .expect("typed_action_executor lock poisoned")
            .cancel_session(&session_id);

        // The client is gone. If it disappeared mid-drag -- connection dropped,
        // app swiped away, phone locked -- a mouse button can still be latched
        // down on the Mac with no touch anywhere able to release it, leaving the
        // machine in a live drag/selection session unattended. Clearing
        // remote-control state here is the only place that can notice.
        crate::remote_input::release_all();
        self.bridge.clear_remote_control_active();

        tinfoil_tasks.abort_all();
        while tinfoil_tasks.join_next().await.is_some() {}

        drop(events_tx);
        let _ = send_task.await;
        connection.closed().await;
        Ok(())
    }
}

/// Shared handle type, for call sites that want to clone-and-store the
/// channel behind an `Arc`, rather than relying on `ControlChannel`'s own
/// `Clone` (which is cheap: it only clones an `Arc<HoloBridge>`).
/// `main.rs` does not use this type today; it clones `ControlChannel`
/// directly. This type stays as a documented convenience alias for
/// callers that prefer an explicit `Arc` (for example, storing it in a
/// struct field alongside other `Arc`-wrapped daemon state).
#[allow(dead_code)]
pub type SharedControlChannel = Arc<ControlChannel>;
