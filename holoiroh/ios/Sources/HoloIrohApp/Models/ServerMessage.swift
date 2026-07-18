import Foundation

/// Swift mirror of `PROTOCOL.md`'s `ServerMessage` (Mac daemon -> iOS),
/// a tagged, internally-tagged enum keyed on `type` with an optional
/// sibling `text` field.
///
/// Wire examples (see ../../../PROTOCOL.md):
/// ```json
/// { "type": "ack" }
/// { "type": "status", "text": "connected to holo-desktop-cli" }
/// { "type": "task_progress", "text": "clicked Safari icon in the Dock" }
/// { "type": "error", "text": "holo-desktop-cli exited unexpectedly (code 1)" }
/// ```
///
/// This type is not yet wired to any real control-channel transport (no
/// networking exists in this skeleton) -- it exists so the status/log
/// panel has a concrete, protocol-shaped model to render today, and so a
/// future control-channel implementation can decode real `ServerMessage`
/// JSON directly into this type via `Codable` without another rewrite of
/// the UI layer.
enum ServerMessage: Codable, Equatable {
    case ack(text: String?)
    case status(text: String?)
    case error(text: String?)
    case taskProgress(text: String?)

    private enum CodingKeys: String, CodingKey {
        case type
        case text
    }

    private enum Kind: String, Codable {
        case ack
        case status
        case error
        case taskProgress = "task_progress"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let kind = try container.decode(Kind.self, forKey: .type)
        let text = try container.decodeIfPresent(String.self, forKey: .text)
        switch kind {
        case .ack: self = .ack(text: text)
        case .status: self = .status(text: text)
        case .error: self = .error(text: text)
        case .taskProgress: self = .taskProgress(text: text)
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .ack(let text):
            try container.encode(Kind.ack, forKey: .type)
            try container.encodeIfPresent(text, forKey: .text)
        case .status(let text):
            try container.encode(Kind.status, forKey: .type)
            try container.encodeIfPresent(text, forKey: .text)
        case .error(let text):
            try container.encode(Kind.error, forKey: .type)
            try container.encodeIfPresent(text, forKey: .text)
        case .taskProgress(let text):
            try container.encode(Kind.taskProgress, forKey: .type)
            try container.encodeIfPresent(text, forKey: .text)
        }
    }

    /// Human-readable text for the status/log panel, falling back to a
    /// label derived from the discriminant when `text` is absent (e.g.
    /// bare `{"type":"ack"}`).
    var displayText: String {
        switch self {
        case .ack(let text): return text ?? "ack"
        case .status(let text): return text ?? "status"
        case .error(let text): return text ?? "error"
        case .taskProgress(let text): return text ?? "task in progress"
        }
    }

    /// Short label for the discriminant, used as a prefix/badge in the
    /// log list so the user can distinguish message kinds at a glance.
    var kindLabel: String {
        switch self {
        case .ack: return "ACK"
        case .status: return "STATUS"
        case .error: return "ERROR"
        case .taskProgress: return "PROGRESS"
        }
    }
}

/// Swift mirror of `PROTOCOL.md`'s `ClientMessage` (iOS -> Mac daemon).
/// Not yet sent over any real transport -- see `ServerMessage`'s doc
/// comment above for the same rationale.
enum ClientMessage: Codable, Equatable {
    case prompt(text: String)
    case voiceTranscript(text: String)
    case stop

    private enum CodingKeys: String, CodingKey {
        case type
        case text
    }

    private enum Kind: String, Codable {
        case prompt
        case voiceTranscript = "voice_transcript"
        case stop
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let kind = try container.decode(Kind.self, forKey: .type)
        switch kind {
        case .prompt:
            self = .prompt(text: try container.decode(String.self, forKey: .text))
        case .voiceTranscript:
            self = .voiceTranscript(text: try container.decode(String.self, forKey: .text))
        case .stop:
            self = .stop
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .prompt(let text):
            try container.encode(Kind.prompt, forKey: .type)
            try container.encode(text, forKey: .text)
        case .voiceTranscript(let text):
            try container.encode(Kind.voiceTranscript, forKey: .type)
            try container.encode(text, forKey: .text)
        case .stop:
            try container.encode(Kind.stop, forKey: .type)
        }
    }
}

/// One entry in the status/log panel. Wraps a `ServerMessage` with a
/// stable identity and timestamp so it can drive a SwiftUI `List`/
/// `ForEach` and be displayed in chronological order.
struct LogEntry: Identifiable, Equatable {
    let id: UUID
    let timestamp: Date
    let message: ServerMessage

    init(id: UUID = UUID(), timestamp: Date = Date(), message: ServerMessage) {
        self.id = id
        self.timestamp = timestamp
        self.message = message
    }

    /// Formatted `HH:mm:ss` timestamp for compact display in the log row.
    var formattedTime: String {
        let formatter = DateFormatter()
        formatter.dateFormat = "HH:mm:ss"
        return formatter.string(from: timestamp)
    }
}
