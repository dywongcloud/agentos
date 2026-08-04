import Foundation

/// Encodes the unsigned envelope that Swift passes to the bridge.
/// Swift supplies framing and typed payload fields, but no signature.
/// The bridge validates the envelope and signs it with the endpoint identity.
/// It serializes the signed envelope immediately before the control-channel write.
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

    /// Uses protocol version 1 and a 30,000-millisecond expiry.
    /// These values match `TaskEnvelope::new`.
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

/// Sends `ClientMessage` values from the app to the daemon over the control channel.
/// The user interface depends on this protocol instead of a concrete transport.
///
/// `FFIControlChannelSender` is the live implementation.
/// It uses the bridge handle that the media subscription uses.
/// After the handshake, `HoloConnection` injects the sender into `MainView.controlChannel`.
/// The sender writes envelope-wrapped newline-delimited JavaScript Object Notation (NDJSON).
/// It uses the bridge's serial foreign function interface (FFI) queue.
///
/// `LoggingControlChannelSender` is the pre-connect and bridge-less fallback.
/// It encodes the same envelope form and reports it instead of writing to a socket.
protocol ControlChannelSending {
    /// Sends one `ClientMessage` as an envelope-wrapped NDJSON value.
    func send(_ message: ClientMessage)
}

/// Tracks the outbound session identifier and sequence number for one connection.
/// The daemon provides the session identifier in its verified greeting.
/// Sequence numbers increase monotonically.
/// A lock serializes access across user-interface and bridge queues.
/// The class lets a sender and log mirror share state.
final class OutboundEnvelopeState {
    /// Remains `nil` until the app receives the daemon greeting.
    /// The lock protects reads and writes across user-interface and bridge queues.
    private var _sessionId: String?
    private var nextSequenceNumber: UInt64 = 0
    private let lock = NSLock()

    init(sessionId: String? = nil) {
        self._sessionId = sessionId
    }

    var sessionId: String? {
        get { lock.lock(); defer { lock.unlock() }; return _sessionId }
        set {
            lock.lock()
            defer { lock.unlock() }
            if _sessionId != newValue {
                nextSequenceNumber = 0
            }
            _sessionId = newValue
        }
    }

    /// Returns the current sequence number and increments the counter.
    /// The method is thread-safe.
    func nextSequence() -> UInt64 {
        lock.lock()
        defer { lock.unlock() }
        let n = nextSequenceNumber
        nextSequenceNumber += 1
        return n
    }
}

extension ControlChannelSending {
    /// Encodes the unsigned typed envelope that the bridge accepts.
    /// The bridge rejects bare payloads and caller-supplied signatures.
    /// It requires a verified session identifier and attaches the endpoint signature.
    /// Returns `nil` before the greeting supplies a session or when JSON encoding fails.
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

/// Encodes messages before a real control-channel connection exists.
/// It reports a human-readable status instead of writing to a socket.
/// A synthetic session identifier makes the envelope shape match the live sender.
struct LoggingControlChannelSender: ControlChannelSending {
    /// Receives each message and its encoded envelope for presentation.
    let report: (ClientMessage, _ wire: String) -> Void

    /// Provides a synthetic session identifier because no daemon greeting exists.
    private let sessionState = OutboundEnvelopeState(sessionId: "logging-stand-in")

    func send(_ message: ClientMessage) {
        guard let wire = encoded(message, sessionState: sessionState) else { return }
        report(message, wire)
    }
}
