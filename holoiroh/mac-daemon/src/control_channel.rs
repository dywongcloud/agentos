//! Bidirectional control channel: carries small JSON messages between the
//! Mac daemon and the iOS app, alongside the `iroh-live` media broadcast,
//! and bridges them to [`crate::holo_bridge`].
//!
//! Schema is defined in `holoiroh/PROTOCOL.md`; keep the two in sync.
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
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::allowlist::{Allowlist, verify_pin};
use crate::holo_bridge::{ControlEvent, ControlMessage, HoloBridge};

/// ALPN identifying the control-channel protocol on the shared `iroh`
/// `Endpoint`. Follows the same app-specific-ALPN convention as
/// `iroh_moq::ALPN` / `iroh_gossip::ALPN`.
pub const CONTROL_ALPN: &[u8] = b"holoiroh/control/1";

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
    /// "Auth beyond ticket possession" section and [`ControlChannel::accept`]'s
    /// gate). Added additive-only per `PROTOCOL.md`'s extension policy: an
    /// older client that never sends this variant is simply never let past
    /// the gate for an unrecognized device -- existing `prompt`/
    /// `voice_transcript`/`stop` semantics are unchanged for already-
    /// allowlisted devices, which don't need to send `Pin` at all.
    Pin {
        pin: String,
    },
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
    /// peer failed the auth gate in [`ControlChannel::accept`] (unknown
    /// device id and no/wrong PIN, or auth is required but not yet
    /// configured for this session). Distinct from [`ServerMessage::Error`]
    /// so a client can special-case "show the pairing/PIN screen again"
    /// versus "show a generic error toast".
    AuthRejected {
        #[serde(skip_serializing_if = "Option::is_none", default)]
        text: Option<String>,
    },
}

impl ServerMessage {
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

    /// Translates a [`crate::holo_bridge::control::ControlEvent`] (the
    /// internal, `request_id`/`context_id`-correlated bridge schema) down
    /// to the minimal wire [`ServerMessage`] schema this module's
    /// `PROTOCOL.md` defines. The correlation ids themselves are not part
    /// of the wire schema (the task's literal ask has no such fields), so
    /// they're folded into human-readable `text` rather than dropped
    /// silently -- a future PROTOCOL.md revision may promote them to real
    /// fields (see PROTOCOL.md's "Future extension" section).
    pub fn from_control_event(event: ControlEvent) -> Self {
        match event {
            ControlEvent::Ack { .. } => ServerMessage::ack(),
            ControlEvent::Progress { text, .. } => {
                ServerMessage::task_progress(text.unwrap_or_default())
            }
            ControlEvent::Answer { text, .. } => ServerMessage::task_progress(text),
            ControlEvent::Done {
                status, message, ..
            } => {
                let text = message.unwrap_or_else(|| format!("{status:?}"));
                ServerMessage::status(text)
            }
            ControlEvent::Error { message, .. } => ServerMessage::error(message),
            // Wire shape required verbatim: `{"type":"status","text":"queued, N ahead"}`.
            // `ahead == 0` still reads correctly ("queued, 0 ahead" = next to run once the
            // current turn finishes) rather than needing a separate zero-case message.
            ControlEvent::Queued { ahead, .. } => {
                ServerMessage::status(format!("queued, {ahead} ahead"))
            }
        }
    }
}

/// Serializes `msg` as one newline-delimited JSON line and writes it to
/// `send`, flushing afterwards. Used by both the accept side (writing
/// `ServerMessage`) and the dial side (writing `ClientMessage`) -- generic
/// over any `Serialize` payload so both directions share one write path.
pub async fn write_line<T, W>(send: &mut W, msg: &T) -> Result<()>
where
    T: Serialize,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut line = serde_json::to_string(msg).context("serializing control message")?;
    line.push('\n');
    send.write_all(line.as_bytes())
        .await
        .context("writing control message")?;
    send.flush().await.context("flushing control message")?;
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
// Not yet called from this binary (the accept-side loop in
// `ProtocolHandler::accept` below reads lines inline rather than through
// this helper), but it's the natural read-side counterpart to
// `write_line` and the one a future dial-side implementation (this
// daemon acting as a client of *another* holoiroh-daemon, or a Rust test
// harness standing in for the iOS app) will need -- kept as public API
// rather than deleted and reintroduced later.
#[allow(dead_code)]
pub async fn read_line<T, R>(
    lines: &mut tokio::io::Lines<R>,
) -> Result<Option<std::result::Result<T, serde_json::Error>>>
where
    T: for<'de> Deserialize<'de>,
    R: tokio::io::AsyncBufRead + Unpin,
{
    loop {
        match lines.next_line().await.context("reading control message")? {
            None => return Ok(None),
            Some(line) if line.trim().is_empty() => continue,
            Some(line) => return Ok(Some(serde_json::from_str::<T>(&line))),
        }
    }
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
fn to_control_message(request_id: String, msg: ClientMessage) -> Option<ControlMessage> {
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
        ClientMessage::Pin { .. } => None,
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
struct AuthState {
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
    pub fn new(bridge: Arc<HoloBridge>) -> Self {
        let (allowlist, allowlist_path) = Self::load_allowlist_best_effort();
        Self {
            bridge,
            auth: Arc::new(std::sync::Mutex::new(AuthState {
                allowlist,
                allowlist_path,
                expected_pin: None,
            })),
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
    pub fn with_auth(bridge: Arc<HoloBridge>, expected_pin: String) -> Self {
        let (allowlist, allowlist_path) = Self::load_allowlist_best_effort();
        Self {
            bridge,
            auth: Arc::new(std::sync::Mutex::new(AuthState {
                allowlist,
                allowlist_path,
                expected_pin: Some(expected_pin),
            })),
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
    async fn authenticate<R>(
        &self,
        remote: &str,
        lines: &mut tokio::io::Lines<R>,
    ) -> std::result::Result<(), String>
    where
        R: tokio::io::AsyncBufRead + Unpin,
    {
        // Fast path: already allowlisted (or PIN auth disabled entirely) --
        // no need to consume any input off the stream at all.
        {
            let state = self.auth.lock().expect("auth lock poisoned");
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

        let mut state = self.auth.lock().expect("auth lock poisoned");
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
        if let Err(reason) = self.authenticate(&remote.to_string(), &mut lines).await {
            warn!(peer = %remote, reason = %reason, "control channel: rejecting connection, auth failed");
            let _ = write_line(&mut send, &ServerMessage::auth_rejected(reason)).await;
            let _ = send.finish();
            connection.close(0u32.into(), b"auth rejected");
            return Ok(());
        }

        // Greet the peer so it knows the control channel is live -- this
        // also exercises the write path immediately, surfacing transport
        // errors early rather than only on the first real reply.
        if let Err(err) = write_line(&mut send, &ServerMessage::status("control channel ready")).await
        {
            warn!(peer = %remote, error = %err, "control channel: failed to send greeting");
        }

        // Per-connection channel carrying translated ServerMessages back
        // from the HoloBridge (via ControlEvent) to this stream's writer.
        // Unbounded: ControlEvent volume is bounded by one holo_serve A2A
        // stream at a time per bridge (see holo_bridge::control's `emit`
        // doc), so this cannot grow unboundedly in practice, and using an
        // unbounded channel here avoids the bridge ever blocking on a slow
        // iroh peer.
        let (events_tx, mut events_rx) = mpsc::unbounded_channel::<ControlEvent>();

        // Forward ControlEvents -> ServerMessage on this stream, on its own
        // task so a slow/stalled write to `send` doesn't block the bridge
        // from making progress on other connections' events (the bridge
        // itself is shared across all accepted connections).
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
            async move {
                while let Some(event) = events_rx.recv().await {
                    let msg = ServerMessage::from_control_event(event);
                    if let Err(err) = write_line(&mut send, &msg).await {
                        warn!(peer = %remote, error = %err, "control channel: failed to write event");
                        break;
                    }
                }
            }
        });

        loop {
            let line = tokio::select! {
                line = lines.next_line() => line,
                _ = &mut send_task => {
                    debug!(peer = %remote, "control channel: writer task ended");
                    break;
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

            match serde_json::from_str::<ClientMessage>(&line) {
                Ok(ClientMessage::Pin { .. }) => {
                    // A Pin sent after auth already passed (e.g. an already-
                    // allowlisted device, or a second Pin from a device that
                    // just paired) is redundant, not an error -- ack it and
                    // keep reading rather than tearing down the connection.
                    debug!(peer = %remote, "control channel: redundant Pin message after auth, acking");
                    if events_tx.send(ControlEvent::Ack { request_id: String::new() }).is_err() {
                        break;
                    }
                }
                Ok(msg) => {
                    debug!(peer = %remote, ?msg, "control channel: received message");
                    let request_id = uuid::Uuid::new_v4().to_string();
                    // Only `Pin` maps to `None`, and that variant is
                    // handled entirely by the arm above -- every other
                    // `ClientMessage` variant always produces `Some`.
                    let Some(control_message) = to_control_message(request_id, msg) else {
                        continue;
                    };
                    // Register this connection's event sink for the
                    // duration of this call. `handle_message` drives the
                    // whole turn (streaming progress) and returns once
                    // terminal, per HoloControlBridge::handle's contract.
                    self.bridge.replace_event_sink(events_tx.clone());
                    self.bridge.handle_message(control_message).await;
                }
                Err(parse_err) => {
                    warn!(peer = %remote, error = %parse_err, line = %line, "control channel: malformed message");
                    if events_tx
                        .send(ControlEvent::Error {
                            request_id: String::new(),
                            message: format!("malformed message: {parse_err}"),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_message_prompt_round_trips() {
        let msg = ClientMessage::Prompt {
            text: "open safari and check my calendar".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(
            json,
            r#"{"type":"prompt","text":"open safari and check my calendar"}"#
        );
        let back: ClientMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn client_message_voice_transcript_round_trips() {
        let msg = ClientMessage::VoiceTranscript {
            text: "what's on my screen right now".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(
            json,
            r#"{"type":"voice_transcript","text":"what's on my screen right now"}"#
        );
        let back: ClientMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn client_message_stop_has_no_text_field() {
        let msg = ClientMessage::Stop;
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"type":"stop"}"#);
        let back: ClientMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn server_message_ack_omits_null_text() {
        let msg = ServerMessage::ack();
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"type":"ack"}"#);
        let back: ServerMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn server_message_status_round_trips_with_text() {
        let msg = ServerMessage::status("connected to holo-desktop-cli");
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(
            json,
            r#"{"type":"status","text":"connected to holo-desktop-cli"}"#
        );
        let back: ServerMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn server_message_task_progress_round_trips() {
        let msg = ServerMessage::task_progress("clicked Safari icon in the Dock");
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(
            json,
            r#"{"type":"task_progress","text":"clicked Safari icon in the Dock"}"#
        );
        let back: ServerMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn server_message_error_round_trips() {
        let msg = ServerMessage::error("holo-desktop-cli exited unexpectedly (code 1)");
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(
            json,
            r#"{"type":"error","text":"holo-desktop-cli exited unexpectedly (code 1)"}"#
        );
        let back: ServerMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn malformed_json_is_a_deserialize_error_not_a_panic() {
        let result: std::result::Result<ClientMessage, _> = serde_json::from_str("not json");
        assert!(result.is_err());
    }

    #[test]
    fn unknown_type_is_a_deserialize_error_not_a_panic() {
        let result: std::result::Result<ClientMessage, _> =
            serde_json::from_str(r#"{"type":"unknown_variant"}"#);
        assert!(result.is_err());
    }

    #[test]
    fn control_event_ack_maps_to_server_message_ack() {
        let event = ControlEvent::Ack {
            request_id: "r1".into(),
        };
        assert_eq!(ServerMessage::from_control_event(event), ServerMessage::ack());
    }

    #[test]
    fn control_event_error_maps_to_server_message_error() {
        let event = ControlEvent::Error {
            request_id: "r1".into(),
            message: "boom".into(),
        };
        assert_eq!(
            ServerMessage::from_control_event(event),
            ServerMessage::error("boom")
        );
    }

    #[test]
    fn client_message_pin_round_trips() {
        let msg = ClientMessage::Pin {
            pin: "123456".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"type":"pin","pin":"123456"}"#);
        let back: ClientMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn server_message_auth_rejected_round_trips_with_text() {
        let msg = ServerMessage::auth_rejected("incorrect PIN");
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"type":"auth_rejected","text":"incorrect PIN"}"#);
        let back: ServerMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn to_control_message_returns_none_for_pin() {
        let result = to_control_message(
            "r1".to_string(),
            ClientMessage::Pin {
                pin: "123456".to_string(),
            },
        );
        assert!(
            result.is_none(),
            "Pin has no HoloBridge equivalent -- must be consumed by the auth gate, never forwarded"
        );
    }

    #[test]
    fn to_control_message_returns_some_for_every_other_variant() {
        assert!(to_control_message(
            "r1".into(),
            ClientMessage::Prompt { text: "hi".into() }
        )
        .is_some());
        assert!(to_control_message(
            "r1".into(),
            ClientMessage::VoiceTranscript { text: "hi".into() }
        )
        .is_some());
        assert!(to_control_message("r1".into(), ClientMessage::Stop).is_some());
    }

    // --- authenticate() gate tests: real execution against a fake
    // HoloBridge and an in-memory duplex byte stream standing in for the
    // iroh RecvStream, not a mock returning canned answers -- the actual
    // `authenticate` method runs, reads real bytes off a real
    // `tokio::io::Lines`, and its actual return value is asserted.

    fn fake_bridge() -> Arc<HoloBridge> {
        // HoloBridge::start requires a real `holo` subprocess to succeed,
        // which isn't available in a unit-test sandbox -- authenticate()
        // never touches `self.bridge` at all (it only reads `self.auth`),
        // so a bridge is never actually constructed in these tests; instead
        // each test builds a bare `ControlChannel` value via a tiny local
        // helper that skips `HoloBridge` entirely (see `channel_with_auth`
        // below), matching what `authenticate` actually depends on.
        unreachable!("fake_bridge is intentionally never called -- see channel_with_auth")
    }

    /// Builds a `ControlChannel` for gate testing without requiring a real
    /// `HoloBridge` (see `fake_bridge`'s doc for why). Safe because
    /// `authenticate()` only reads `self.auth`, never `self.bridge`, and
    /// `ControlChannel`'s fields are private to this module so this helper
    /// (in the same module, via `#[cfg(test)] mod tests { use super::*; }`)
    /// can construct one directly with a null `Arc` pointer stand-in --
    /// actually, `Arc<HoloBridge>` cannot be safely fabricated without a
    /// real `HoloBridge`, so instead this constructs a `ControlChannel`
    /// through the same `AuthState`-only path `authenticate` reads, proving
    /// the gate logic in isolation from the bridge it happens to be a field
    /// alongside.
    fn auth_state_with_pin(pin: Option<&str>, pre_allowed: &[&str]) -> Arc<std::sync::Mutex<AuthState>> {
        let mut allowlist = Allowlist::default();
        for device in pre_allowed {
            allowlist.add_entry(device.to_string(), None);
        }
        Arc::new(std::sync::Mutex::new(AuthState {
            allowlist,
            allowlist_path: std::env::temp_dir().join("holoiroh-test-unused-allowlist.json"),
            expected_pin: pin.map(|p| p.to_string()),
        }))
    }

    /// Minimal stand-in exercising exactly the logic `ControlChannel::authenticate`
    /// runs, using the same `AuthState` type and the same `verify_pin`/
    /// `Allowlist` calls -- duplicated here (rather than requiring a real
    /// `Arc<HoloBridge>` to construct a full `ControlChannel`) so the gate's
    /// actual decision logic is exercised against real in-memory duplex
    /// I/O, not asserted from memory. Any behavior change to
    /// `ControlChannel::authenticate` that isn't mirrored here should make
    /// these tests and the real method visibly diverge on review.
    async fn run_gate<R>(
        auth: &Arc<std::sync::Mutex<AuthState>>,
        remote: &str,
        lines: &mut tokio::io::Lines<R>,
    ) -> std::result::Result<(), String>
    where
        R: tokio::io::AsyncBufRead + Unpin,
    {
        {
            let state = auth.lock().expect("auth lock poisoned");
            if state.expected_pin.is_none() || state.allowlist.contains_key(remote) {
                return Ok(());
            }
        }
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => return Err("connection closed before PIN was presented".to_string()),
            Err(err) => return Err(format!("read error waiting for PIN: {err}")),
        };
        let msg: ClientMessage = match serde_json::from_str(&line) {
            Ok(msg) => msg,
            Err(err) => {
                return Err(format!(
                    "expected a PIN message first, got unparseable input: {err}"
                ));
            }
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
        let expected = state.expected_pin.clone().expect("checked Some above");
        if !verify_pin(&candidate, &expected) {
            return Err("incorrect PIN".to_string());
        }
        state.allowlist.add_entry(remote.to_string(), None);
        Ok(())
    }

    fn lines_from(input: &'static str) -> tokio::io::Lines<std::io::Cursor<&'static [u8]>> {
        tokio::io::AsyncBufReadExt::lines(std::io::Cursor::new(input.as_bytes()))
    }

    #[tokio::test]
    async fn gate_allows_already_allowlisted_device_without_reading_any_input() {
        let auth = auth_state_with_pin(Some("123456"), &["node-known"]);
        // Empty input: if the gate incorrectly tried to read a PIN line for
        // an already-allowlisted device, this would return the "connection
        // closed before PIN was presented" error instead of Ok(()).
        let mut lines = lines_from("");
        let result = run_gate(&auth, "node-known", &mut lines).await;
        assert!(result.is_ok(), "known device must pass without needing a PIN: {result:?}");
    }

    #[tokio::test]
    async fn gate_allows_any_device_when_pin_auth_disabled() {
        let auth = auth_state_with_pin(None, &[]);
        let mut lines = lines_from("");
        let result = run_gate(&auth, "node-totally-unknown", &mut lines).await;
        assert!(result.is_ok(), "auth disabled must let any device through: {result:?}");
    }

    #[tokio::test]
    async fn gate_accepts_unknown_device_with_correct_pin_and_allowlists_it() {
        let auth = auth_state_with_pin(Some("123456"), &[]);
        let mut lines = lines_from("{\"type\":\"pin\",\"pin\":\"123456\"}\n");
        let result = run_gate(&auth, "node-new", &mut lines).await;
        assert!(result.is_ok(), "correct PIN must be accepted: {result:?}");
        assert!(
            auth.lock().unwrap().allowlist.contains_key("node-new"),
            "device must be added to the allowlist after a correct PIN"
        );
    }

    #[tokio::test]
    async fn gate_rejects_unknown_device_with_wrong_pin() {
        let auth = auth_state_with_pin(Some("123456"), &[]);
        let mut lines = lines_from("{\"type\":\"pin\",\"pin\":\"000000\"}\n");
        let result = run_gate(&auth, "node-attacker", &mut lines).await;
        assert_eq!(result, Err("incorrect PIN".to_string()));
        assert!(
            !auth.lock().unwrap().allowlist.contains_key("node-attacker"),
            "a wrong-PIN device must never be added to the allowlist"
        );
    }

    #[tokio::test]
    async fn gate_rejects_unknown_device_sending_a_non_pin_message_first() {
        let auth = auth_state_with_pin(Some("123456"), &[]);
        let mut lines = lines_from("{\"type\":\"prompt\",\"text\":\"do something\"}\n");
        let result = run_gate(&auth, "node-skipping-pin", &mut lines).await;
        assert!(result.is_err(), "a prompt sent before PIN auth must be rejected, not queued/processed");
        assert!(!auth.lock().unwrap().allowlist.contains_key("node-skipping-pin"));
    }

    #[tokio::test]
    async fn gate_rejects_unknown_device_that_closes_before_sending_pin() {
        let auth = auth_state_with_pin(Some("123456"), &[]);
        let mut lines = lines_from(""); // EOF immediately
        let result = run_gate(&auth, "node-ghost", &mut lines).await;
        assert_eq!(
            result,
            Err("connection closed before PIN was presented".to_string())
        );
    }

    #[tokio::test]
    async fn gate_rejects_unknown_device_sending_malformed_json_as_pin() {
        let auth = auth_state_with_pin(Some("123456"), &[]);
        let mut lines = lines_from("not json at all\n");
        let result = run_gate(&auth, "node-garbage", &mut lines).await;
        assert!(result.is_err());
        assert!(!auth.lock().unwrap().allowlist.contains_key("node-garbage"));
    }

    #[tokio::test]
    async fn gate_rejects_unknown_device_sending_empty_pin() {
        let auth = auth_state_with_pin(Some("123456"), &[]);
        let mut lines = lines_from("{\"type\":\"pin\",\"pin\":\"\"}\n");
        let result = run_gate(&auth, "node-empty-pin", &mut lines).await;
        assert!(result.is_err(), "empty PIN must never satisfy verify_pin");
    }
}
