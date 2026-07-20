import Foundation

/// Swift mirror of the daemon's `TaskEnvelope<T>` (`control_channel.rs`), the
/// wrapper every wire message -- both directions -- must carry once a
/// `session_id` exists (i.e. everything except the pre-session PIN line).
/// Field set and JSON key names match `control_channel.rs`'s `TaskEnvelope`
/// exactly: an inbound line missing any non-optional field here is rejected
/// by the daemon with "malformed envelope: missing field `<name>`" -- this
/// struct's shape IS the fix for that failure mode, not a convenience.
private struct OutboundEnvelope: Encodable {
    let protocolVersion: UInt32
    let messageId: String
    let sessionId: String
    let taskId: String?
    let messageType: String
    let sentAt: UInt64
    let expiresAt: UInt64
    let sequenceNumber: UInt64
    let payload: ClientMessage
    let signature: String?

    private enum CodingKeys: String, CodingKey {
        case protocolVersion = "protocol_version"
        case messageId = "message_id"
        case sessionId = "session_id"
        case taskId = "task_id"
        case messageType = "message_type"
        case sentAt = "sent_at"
        case expiresAt = "expires_at"
        case sequenceNumber = "sequence_number"
        case payload
        case signature
    }

    /// Matches `TaskEnvelope::new`'s `protocol_version`/expiry -- see
    /// `control_channel.rs`'s `PROTOCOL_VERSION` (1) and `DEFAULT_EXPIRY_MS`
    /// (30_000) constants.
    static let protocolVersion: UInt32 = 1
    static let defaultExpiryMs: UInt64 = 30_000

    init(sessionId: String, sequenceNumber: UInt64, payload: ClientMessage) {
        let sentAt = UInt64(Date().timeIntervalSince1970 * 1000)
        self.protocolVersion = Self.protocolVersion
        self.messageId = UUID().uuidString
        self.sessionId = sessionId
        self.taskId = nil
        self.messageType = payload.wireKindLabel
        self.sentAt = sentAt
        self.expiresAt = sentAt + Self.defaultExpiryMs
        self.sequenceNumber = sequenceNumber
        self.payload = payload
        self.signature = nil
    }
}

/// The seam through which the app sends a `ClientMessage` (`Models/ServerMessage.swift`)
/// to the Mac daemon over the control channel.
///
/// This is the single injection point the UI talks to when it needs to *send*
/// something -- most importantly the remote **kill-switch** `ClientMessage.stop`
/// wired to the Working/Connecting "Cancel" control (see `MainView.sessionActions`).
/// Keeping it a protocol means the view layer never references the transport
/// directly: today it is handed the `LoggingControlChannelSender` stand-in, and
/// the day the real `iroh` transport exists it is handed a different conforming
/// type at exactly one place (`MainView`'s initializer default) with no change to
/// any button, panel, or action closure.
///
/// ## The real transport
///
/// The real conforming type is `FFIControlChannelSender`
/// (`HoloConnection.swift`): it holds the opaque bridge handle from
/// ticket-connect (the same handle the live-video subscription uses) and
/// writes each message's `encoded(_:)` NDJSON line to the daemon via
/// `holoiroh_ios_bridge_control_send`, on the connection's serial FFI queue.
/// It is injected at `MainView.controlChannel` the moment `HoloConnection`
/// completes the control-ALPN PIN handshake.
///
/// `LoggingControlChannelSender` remains as the pre-connect / bridge-less
/// fallback: it performs the *encode* half for real (so the wire bytes are
/// exercised and witnessable) and reports the message to the status/log
/// panel instead of putting it on a socket.
protocol ControlChannelSending {
    /// Send one `ClientMessage` to the daemon. Implementations must encode it as
    /// the envelope-wrapped NDJSON wire form; see `ControlChannelSending.encoded(_:sessionState:)`.
    func send(_ message: ClientMessage)
}

/// Per-connection outbound envelope state: the `session_id` the daemon minted
/// for this connection (learned from its envelope-wrapped greeting -- see
/// `HoloConnection.decodeServerLine`/`sessionId(from:)`) plus a monotonically
/// increasing `sequence_number`. Mirrors the daemon's own
/// `OutboundEnvelopeState` (`control_channel.rs`) on the other side of the
/// wire. A class (not a struct) so `FFIControlChannelSender` and any log
/// mirror of it share one counter by reference.
final class OutboundEnvelopeState {
    /// `nil` until the daemon's greeting is observed. A send attempted before
    /// then has no valid envelope to build -- see `encoded(_:sessionState:)`.
    /// Lock-protected: written from `HoloConnection.decodeServerLine` (on the
    /// serial FFI queue) and read from `encoded(_:sessionState:)`, which can
    /// run on whatever thread a UI action calls `send(_:)` from -- see
    /// `FFIControlChannelSender.send`.
    private var _sessionId: String?
    private var nextSequenceNumber: UInt64 = 0
    private let lock = NSLock()

    init(sessionId: String? = nil) {
        self._sessionId = sessionId
    }

    var sessionId: String? {
        get { lock.lock(); defer { lock.unlock() }; return _sessionId }
        set { lock.lock(); defer { lock.unlock() }; _sessionId = newValue }
    }

    /// Returns this send's sequence number and advances the counter.
    /// Thread-safe: `FFIControlChannelSender.send` can be called from any
    /// queue that then hops to the serial FFI queue.
    func nextSequence() -> UInt64 {
        lock.lock()
        defer { lock.unlock() }
        let n = nextSequenceNumber
        nextSequenceNumber += 1
        return n
    }
}

extension ControlChannelSending {
    /// The exact newline-delimited-JSON wire bytes for `message`, wrapped in
    /// the `TaskEnvelope` shape the daemon's `control_channel.rs` requires
    /// for every post-greeting inbound line (see `OutboundEnvelope`'s doc for
    /// exactly why: omitting this wrapper is what produced "malformed
    /// envelope: missing field `protocol_version`"). `sessionState.sessionId`
    /// must already be populated from the daemon's greeting -- returns `nil`
    /// (nothing to send) if it isn't, same as the pre-existing `JSONEncoder`
    /// failure contract below.
    func encoded(_ message: ClientMessage, sessionState: OutboundEnvelopeState) -> String? {
        guard let sessionId = sessionState.sessionId else { return nil }
        let envelope = OutboundEnvelope(
            sessionId: sessionId,
            sequenceNumber: sessionState.nextSequence(),
            payload: message
        )
        let encoder = JSONEncoder()
        // Deterministic key order so the wire form is stable/inspectable (the
        // daemon parses by key regardless, but a stable form makes logs and
        // packet captures reproducible).
        encoder.outputFormatting = [.sortedKeys]
        guard let data = try? encoder.encode(envelope),
              let json = String(data: data, encoding: .utf8) else {
            return nil
        }
        return json + "\n"
    }
}

/// The default `ControlChannelSending` used until a real connection exists
/// (or the bridge-less/simulator build): it performs the real JSON *encoding*
/// of every `ClientMessage` (exercising the exact envelope-wrapped wire
/// bytes) and forwards a human-readable line to a caller-supplied sink (the
/// status/log panel) instead of writing to a socket.
///
/// This is deliberately not a no-op: encoding the kill-switch `ClientMessage.stop`
/// on every Cancel press means the send path is demonstrably live and the wire
/// contract is exercised today. It carries its own synthetic `OutboundEnvelopeState`
/// (a fixed placeholder `session_id`, since no real daemon session exists here) so
/// the encoded form matches exactly what `FFIControlChannelSender` will later send.
struct LoggingControlChannelSender: ControlChannelSending {
    /// Where a sent message is surfaced. `MainView` passes a closure that appends
    /// a `ServerMessage`-shaped entry to its log panel, so a sent `.stop` shows up
    /// in the same status list as everything else.
    let report: (ClientMessage, _ wire: String) -> Void

    /// Placeholder-session envelope state: this sender never reaches a real
    /// daemon, so there is no real `session_id` to learn from a greeting.
    private let sessionState = OutboundEnvelopeState(sessionId: "logging-stand-in")

    func send(_ message: ClientMessage) {
        guard let wire = encoded(message, sessionState: sessionState) else { return }
        report(message, wire)
    }
}
