import Foundation

/// Swift mirror of `PROTOCOL.md`'s `ServerMessage` (Mac daemon -> iOS),
/// a tagged, internally-tagged enum keyed on `type`.
///
/// Wire examples (see ../../../PROTOCOL.md):
/// ```json
/// { "type": "ack" }
/// { "type": "status", "text": "connected to holo-desktop-cli" }
/// { "type": "task_progress", "text": "clicked Safari icon in the Dock" }
/// { "type": "task_done", "status": "completed", "text": "answer text" }
/// { "type": "error", "text": "holo-desktop-cli exited unexpectedly (code 1)" }
/// { "type": "auth_rejected", "text": "incorrect PIN" }
/// { "type": "input_request", "request_id": "…", "kind": "sensitive_access_consent",
///   "context": "…", "response_options": ["Allow once", "Stop task"], "expires_at": 0 }
/// ```
///
/// Every daemon frame kind must decode here: `HoloConnection.decodeServerLine`
/// falls back to an "unrecognized control event" status line for anything this
/// enum can't decode, which is exactly how `auth_rejected` and `input_request`
/// frames used to silently degrade before these cases were added.
enum ServerMessage: Codable, Equatable {
    case ack(text: String?)
    case status(text: String?)
    case error(text: String?)
    case taskProgress(text: String?)
    /// Terminal lifecycle for one task: `status` is `"completed"`,
    /// `"failed"`, or `"canceled"` (the daemon's `DoneStatus` snake_case).
    /// This is the signal the task-control UI keys off to know a task ended.
    case taskDone(status: String, text: String?)
    /// Sent right after the greeting on a (re)connect when a task from before
    /// the connection drop is still live, so the app can restore its Pause/Stop
    /// task-control pill (in the paused state when `paused`). `queued` is how
    /// many prompts wait behind it. See PROTOCOL.md `task_active`.
    case taskActive(paused: Bool, queued: Int)
    /// The daemon rejected this connection's auth (unknown device / wrong
    /// PIN) and is about to close it.
    case authRejected(text: String?)
    /// The P0-14 structured input request -- today produced by the daemon's
    /// sensitive-app consent gate. `kind` is the wire snake_case kind string
    /// (e.g. `"sensitive_access_consent"`); answer via
    /// `ClientMessage.inputResponse` echoing `requestId` and one of
    /// `responseOptions`.
    case inputRequest(
        requestId: String,
        kind: String,
        context: String,
        responseOptions: [String],
        expiresAt: UInt64
    )

    private enum CodingKeys: String, CodingKey {
        case type
        case text
        case status
        case requestId = "request_id"
        case kind
        case context
        case responseOptions = "response_options"
        case expiresAt = "expires_at"
        case paused
        case queued
    }

    private enum Kind: String, Codable {
        case ack
        case status
        case error
        case taskProgress = "task_progress"
        case taskDone = "task_done"
        case taskActive = "task_active"
        case authRejected = "auth_rejected"
        case inputRequest = "input_request"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let kind = try container.decode(Kind.self, forKey: .type)
        switch kind {
        case .ack:
            self = .ack(text: try container.decodeIfPresent(String.self, forKey: .text))
        case .status:
            self = .status(text: try container.decodeIfPresent(String.self, forKey: .text))
        case .error:
            self = .error(text: try container.decodeIfPresent(String.self, forKey: .text))
        case .taskProgress:
            self = .taskProgress(text: try container.decodeIfPresent(String.self, forKey: .text))
        case .taskDone:
            self = .taskDone(
                status: try container.decode(String.self, forKey: .status),
                text: try container.decodeIfPresent(String.self, forKey: .text)
            )
        case .taskActive:
            self = .taskActive(
                paused: try container.decodeIfPresent(Bool.self, forKey: .paused) ?? false,
                queued: try container.decodeIfPresent(Int.self, forKey: .queued) ?? 0
            )
        case .authRejected:
            self = .authRejected(text: try container.decodeIfPresent(String.self, forKey: .text))
        case .inputRequest:
            self = .inputRequest(
                requestId: try container.decode(String.self, forKey: .requestId),
                kind: try container.decode(String.self, forKey: .kind),
                context: try container.decode(String.self, forKey: .context),
                responseOptions: try container.decode([String].self, forKey: .responseOptions),
                expiresAt: try container.decode(UInt64.self, forKey: .expiresAt)
            )
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
        case .taskDone(let status, let text):
            try container.encode(Kind.taskDone, forKey: .type)
            try container.encode(status, forKey: .status)
            try container.encodeIfPresent(text, forKey: .text)
        case .taskActive(let paused, let queued):
            try container.encode(Kind.taskActive, forKey: .type)
            try container.encode(paused, forKey: .paused)
            try container.encode(queued, forKey: .queued)
        case .authRejected(let text):
            try container.encode(Kind.authRejected, forKey: .type)
            try container.encodeIfPresent(text, forKey: .text)
        case .inputRequest(let requestId, let kind, let context, let responseOptions, let expiresAt):
            try container.encode(Kind.inputRequest, forKey: .type)
            try container.encode(requestId, forKey: .requestId)
            try container.encode(kind, forKey: .kind)
            try container.encode(context, forKey: .context)
            try container.encode(responseOptions, forKey: .responseOptions)
            try container.encode(expiresAt, forKey: .expiresAt)
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
        case .taskDone(let status, let text):
            if let text, !text.isEmpty { return "\(status): \(text)" }
            return status
        case .taskActive(let paused, let queued):
            let base = paused ? "task paused from before" : "task still running from before"
            return queued > 0 ? "\(base) (\(queued) queued)" : base
        case .authRejected(let text): return text ?? "authentication rejected"
        case .inputRequest(_, _, let context, _, _): return context
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
        case .taskDone: return "DONE"
        case .taskActive: return "TASK"
        case .authRejected: return "AUTH"
        case .inputRequest: return "INPUT"
        }
    }
}

/// Swift mirror of `PROTOCOL.md`'s `ClientMessage` (iOS -> Mac daemon).
enum ClientMessage: Codable, Equatable {
    case prompt(text: String)
    case voiceTranscript(text: String)
    case stop
    /// Pause the running task (daemon parks it; `resume` continues it on the
    /// same backend session).
    case pause
    /// Resume the parked task.
    case resume
    /// Replace the running/queued work with a new instruction, keeping the
    /// task's session history.
    case redirect(text: String)
    /// Answer to a `ServerMessage.inputRequest` -- echoes its `requestId`
    /// plus one of its `responseOptions`, verbatim. Never carries free text
    /// or a credential (see the daemon's wire-schema doc: this is a
    /// structured selection only).
    case inputResponse(requestId: String, selectedOption: String)
    /// The user escalated to hands-on control and is driving the Mac directly by
    /// touching the live-share view; `event` is one normalized touch action.
    case remoteControl(RemoteControlEvent)

    private enum CodingKeys: String, CodingKey {
        case type
        case text
        case requestId = "request_id"
        case selectedOption = "selected_option"
        case event
    }

    private enum Kind: String, Codable {
        case prompt
        case voiceTranscript = "voice_transcript"
        case stop
        case pause
        case resume
        case redirect
        case inputResponse = "input_response"
        case remoteControl = "remote_control"
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
        case .pause:
            self = .pause
        case .resume:
            self = .resume
        case .redirect:
            self = .redirect(text: try container.decode(String.self, forKey: .text))
        case .inputResponse:
            self = .inputResponse(
                requestId: try container.decode(String.self, forKey: .requestId),
                selectedOption: try container.decode(String.self, forKey: .selectedOption)
            )
        case .remoteControl:
            self = .remoteControl(try container.decode(RemoteControlEvent.self, forKey: .event))
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
        case .pause:
            try container.encode(Kind.pause, forKey: .type)
        case .resume:
            try container.encode(Kind.resume, forKey: .type)
        case .redirect(let text):
            try container.encode(Kind.redirect, forKey: .type)
            try container.encode(text, forKey: .text)
        case .inputResponse(let requestId, let selectedOption):
            try container.encode(Kind.inputResponse, forKey: .type)
            try container.encode(requestId, forKey: .requestId)
            try container.encode(selectedOption, forKey: .selectedOption)
        case .remoteControl(let event):
            try container.encode(Kind.remoteControl, forKey: .type)
            try container.encode(event, forKey: .event)
        }
    }

    /// Short label for the message's wire `type` discriminant, for the
    /// status/log panel. Mirrors the daemon's `ClientMessage::type_tag`
    /// snake_case discriminants exactly.
    var wireKindLabel: String {
        switch self {
        case .prompt: return "prompt"
        case .voiceTranscript: return "voice_transcript"
        case .stop: return "stop"
        case .pause: return "pause"
        case .resume: return "resume"
        case .redirect: return "redirect"
        case .inputResponse: return "input_response"
        case .remoteControl: return "remote_control"
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
