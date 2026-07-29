import Foundation

/// One clarifying question the daemon generated for an ambiguous instruction
/// (see `ServerMessage.clarifyQuestions`). `options` are concrete suggested
/// answers; the UI renders them as single-select choices and appends its own
/// "Something else…" free-text entry as the final option.
struct ClarifyingQuestion: Codable, Equatable, Identifiable {
    let question: String
    let options: [String]

    var id: String { question }
}

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
    /// The daemon's own current `iroh-live:` ticket, sent right after the
    /// greeting. Lets the app refresh a stored default whose ticket went stale
    /// on an identity rotation, over the already-authenticated channel.
    case currentTicket(ticket: String)
    /// Clarifying questions the daemon generated for a `ClientMessage.clarifyRequest`
    /// -- EMPTY when the instruction was already clear (the app then sends the
    /// prompt directly). Each question carries concrete options; the UI appends
    /// its own "Something else…" free-text option.
    case clarifyQuestions(questions: [ClarifyingQuestion])
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
    /// Whether the Mac's currently-focused field is a secure (password-class) input right
    /// now -- true whenever the login window's authentication UI, a screen-lock password
    /// prompt, or a `sudo`/Keychain dialog has focus. Sent whenever this state CHANGES, not
    /// per-frame. The video's black rectangle over that field is macOS's own
    /// ScreenCaptureKit security boundary (not a bug); this signal lets the app explain it
    /// instead of leaving it unexplained. See `mac-daemon/src/holo_bridge/secure_input_watchdog.rs`.
    case secureInputState(active: Bool)
    /// Successful reply to `ClientMessage.processDocument`.
    case documentProcessed(requestId: String, markdown: String)
    /// Failure reply to `ClientMessage.processDocument`.
    case documentProcessFailed(requestId: String, error: String)
    /// Successful reply to `ClientMessage.analyzeImage`.
    case imageAnalyzed(requestId: String, text: String)
    /// Failure reply to `ClientMessage.analyzeImage`.
    case imageAnalysisFailed(requestId: String, error: String)
    /// Successful reply to `ClientMessage.transcribeAudio`.
    case audioTranscribed(requestId: String, text: String)
    /// Failure reply to `ClientMessage.transcribeAudio`.
    case audioTranscriptionFailed(requestId: String, error: String)
    /// Successful reply to `ClientMessage.requestSpeech`: `audioDataBase64` is WAV bytes.
    case speechReady(requestId: String, audioDataBase64: String)
    /// Failure reply to `ClientMessage.requestSpeech`.
    case speechFailed(requestId: String, error: String)
    /// Successful reply to `ClientMessage.planTask`: an ordered, human-readable step list to
    /// show the user before anything runs -- this is a plan, not an execution.
    case planReady(requestId: String, steps: [String])
    /// Failure reply to `ClientMessage.planTask`.
    case planFailed(requestId: String, error: String)

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
        case ticket
        case questions
        case active
        case markdown
        case error
        case audioDataBase64 = "audio_data_base64"
        case steps
    }

    private enum Kind: String, Codable {
        case ack
        case status
        case error
        case taskProgress = "task_progress"
        case taskDone = "task_done"
        case taskActive = "task_active"
        case authRejected = "auth_rejected"
        case currentTicket = "current_ticket"
        case clarifyQuestions = "clarify_questions"
        case inputRequest = "input_request"
        case secureInputState = "secure_input_state"
        case documentProcessed = "document_processed"
        case documentProcessFailed = "document_process_failed"
        case imageAnalyzed = "image_analyzed"
        case imageAnalysisFailed = "image_analysis_failed"
        case audioTranscribed = "audio_transcribed"
        case audioTranscriptionFailed = "audio_transcription_failed"
        case speechReady = "speech_ready"
        case speechFailed = "speech_failed"
        case planReady = "plan_ready"
        case planFailed = "plan_failed"
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
        case .currentTicket:
            self = .currentTicket(ticket: try container.decode(String.self, forKey: .ticket))
        case .clarifyQuestions:
            self = .clarifyQuestions(
                questions: try container.decodeIfPresent([ClarifyingQuestion].self, forKey: .questions) ?? []
            )
        case .inputRequest:
            self = .inputRequest(
                requestId: try container.decode(String.self, forKey: .requestId),
                kind: try container.decode(String.self, forKey: .kind),
                context: try container.decode(String.self, forKey: .context),
                responseOptions: try container.decode([String].self, forKey: .responseOptions),
                expiresAt: try container.decode(UInt64.self, forKey: .expiresAt)
            )
        case .secureInputState:
            self = .secureInputState(active: try container.decode(Bool.self, forKey: .active))
        case .documentProcessed:
            self = .documentProcessed(
                requestId: try container.decode(String.self, forKey: .requestId),
                markdown: try container.decode(String.self, forKey: .markdown)
            )
        case .documentProcessFailed:
            self = .documentProcessFailed(
                requestId: try container.decode(String.self, forKey: .requestId),
                error: try container.decode(String.self, forKey: .error)
            )
        case .imageAnalyzed:
            self = .imageAnalyzed(
                requestId: try container.decode(String.self, forKey: .requestId),
                text: try container.decode(String.self, forKey: .text)
            )
        case .imageAnalysisFailed:
            self = .imageAnalysisFailed(
                requestId: try container.decode(String.self, forKey: .requestId),
                error: try container.decode(String.self, forKey: .error)
            )
        case .audioTranscribed:
            self = .audioTranscribed(
                requestId: try container.decode(String.self, forKey: .requestId),
                text: try container.decode(String.self, forKey: .text)
            )
        case .audioTranscriptionFailed:
            self = .audioTranscriptionFailed(
                requestId: try container.decode(String.self, forKey: .requestId),
                error: try container.decode(String.self, forKey: .error)
            )
        case .speechReady:
            self = .speechReady(
                requestId: try container.decode(String.self, forKey: .requestId),
                audioDataBase64: try container.decode(String.self, forKey: .audioDataBase64)
            )
        case .speechFailed:
            self = .speechFailed(
                requestId: try container.decode(String.self, forKey: .requestId),
                error: try container.decode(String.self, forKey: .error)
            )
        case .planReady:
            self = .planReady(
                requestId: try container.decode(String.self, forKey: .requestId),
                steps: try container.decode([String].self, forKey: .steps)
            )
        case .planFailed:
            self = .planFailed(
                requestId: try container.decode(String.self, forKey: .requestId),
                error: try container.decode(String.self, forKey: .error)
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
        case .currentTicket(let ticket):
            try container.encode(Kind.currentTicket, forKey: .type)
            try container.encode(ticket, forKey: .ticket)
        case .clarifyQuestions(let questions):
            try container.encode(Kind.clarifyQuestions, forKey: .type)
            try container.encode(questions, forKey: .questions)
        case .inputRequest(let requestId, let kind, let context, let responseOptions, let expiresAt):
            try container.encode(Kind.inputRequest, forKey: .type)
            try container.encode(requestId, forKey: .requestId)
            try container.encode(kind, forKey: .kind)
            try container.encode(context, forKey: .context)
            try container.encode(responseOptions, forKey: .responseOptions)
            try container.encode(expiresAt, forKey: .expiresAt)
        case .secureInputState(let active):
            try container.encode(Kind.secureInputState, forKey: .type)
            try container.encode(active, forKey: .active)
        case .documentProcessed(let requestId, let markdown):
            try container.encode(Kind.documentProcessed, forKey: .type)
            try container.encode(requestId, forKey: .requestId)
            try container.encode(markdown, forKey: .markdown)
        case .documentProcessFailed(let requestId, let error):
            try container.encode(Kind.documentProcessFailed, forKey: .type)
            try container.encode(requestId, forKey: .requestId)
            try container.encode(error, forKey: .error)
        case .imageAnalyzed(let requestId, let text):
            try container.encode(Kind.imageAnalyzed, forKey: .type)
            try container.encode(requestId, forKey: .requestId)
            try container.encode(text, forKey: .text)
        case .imageAnalysisFailed(let requestId, let error):
            try container.encode(Kind.imageAnalysisFailed, forKey: .type)
            try container.encode(requestId, forKey: .requestId)
            try container.encode(error, forKey: .error)
        case .audioTranscribed(let requestId, let text):
            try container.encode(Kind.audioTranscribed, forKey: .type)
            try container.encode(requestId, forKey: .requestId)
            try container.encode(text, forKey: .text)
        case .audioTranscriptionFailed(let requestId, let error):
            try container.encode(Kind.audioTranscriptionFailed, forKey: .type)
            try container.encode(requestId, forKey: .requestId)
            try container.encode(error, forKey: .error)
        case .speechReady(let requestId, let audioDataBase64):
            try container.encode(Kind.speechReady, forKey: .type)
            try container.encode(requestId, forKey: .requestId)
            try container.encode(audioDataBase64, forKey: .audioDataBase64)
        case .speechFailed(let requestId, let error):
            try container.encode(Kind.speechFailed, forKey: .type)
            try container.encode(requestId, forKey: .requestId)
            try container.encode(error, forKey: .error)
        case .planReady(let requestId, let steps):
            try container.encode(Kind.planReady, forKey: .type)
            try container.encode(requestId, forKey: .requestId)
            try container.encode(steps, forKey: .steps)
        case .planFailed(let requestId, let error):
            try container.encode(Kind.planFailed, forKey: .type)
            try container.encode(requestId, forKey: .requestId)
            try container.encode(error, forKey: .error)
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
        case .currentTicket(let ticket): return "daemon ticket: \(ticket)"
        case .clarifyQuestions(let questions):
            return questions.isEmpty ? "no clarification needed" : "\(questions.count) clarifying question(s)"
        case .inputRequest(_, _, let context, _, _): return context
        case .secureInputState(let active):
            return active ? "Mac is at a login/lock screen" : "Mac is signed in"
        case .documentProcessed: return "document processed"
        case .documentProcessFailed(_, let error): return error
        case .imageAnalyzed(_, let text): return text
        case .imageAnalysisFailed(_, let error): return error
        case .audioTranscribed(_, let text): return text
        case .audioTranscriptionFailed(_, let error): return error
        case .speechReady: return "speech ready"
        case .speechFailed(_, let error): return error
        case .planReady(_, let steps): return "\(steps.count) step plan ready"
        case .planFailed(_, let error): return error
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
        case .currentTicket: return "TICKET"
        case .clarifyQuestions: return "CLARIFY"
        case .inputRequest: return "INPUT"
        case .secureInputState: return "LOCK"
        case .documentProcessed, .documentProcessFailed: return "DOCUMENT"
        case .imageAnalyzed, .imageAnalysisFailed: return "IMAGE"
        case .audioTranscribed, .audioTranscriptionFailed: return "TRANSCRIBE"
        case .speechReady, .speechFailed: return "SPEECH"
        case .planReady, .planFailed: return "PLAN"
        }
    }
}

/// Swift mirror of `PROTOCOL.md`'s `ClientMessage` (iOS -> Mac daemon).
enum ClientMessage: Codable, Equatable {
    case prompt(text: String)
    case voiceTranscript(text: String)
    /// The remote kill-switch. `contextId == nil` is the global form
    /// (`{"type":"stop"}`, byte-identical to before): the daemon cancels the
    /// running turn, drains the queue, and engages `holo stop`. A non-nil
    /// `contextId` scopes the cancel to that one A2A context (no queue
    /// drain, no global stop) -- no per-turn ids exist on this client today,
    /// so every Cancel control sends nil.
    case stop(contextId: String?)
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
    /// Asks the daemon to generate clarifying questions for a possibly-ambiguous
    /// instruction before it runs (answered by `ServerMessage.clarifyQuestions`).
    case clarifyRequest(prompt: String)
    /// Asks the daemon to convert an attached document to markdown via Tinfoil
    /// (answered by `ServerMessage.documentProcessed`/`documentProcessFailed`).
    case processDocument(requestId: String, filename: String, dataBase64: String, mode: String)
    /// Asks the daemon to analyze an attached image via Tinfoil (redacted on-device before
    /// upload). Answered by `ServerMessage.imageAnalyzed`/`imageAnalysisFailed`.
    case analyzeImage(requestId: String, imageDataBase64: String, prompt: String)
    /// Asks the daemon to transcribe audio via Tinfoil. **Only ever send audio captured from
    /// this device's own microphone** -- never system/speaker output. Opt-in alternative to
    /// the default on-device `voiceTranscript` path. Answered by
    /// `ServerMessage.audioTranscribed`/`audioTranscriptionFailed`.
    case transcribeAudio(requestId: String, audioDataBase64: String, format: String)
    /// Asks the daemon to synthesize `text` as speech via Tinfoil. Answered by
    /// `ServerMessage.speechReady`/`speechFailed`.
    case requestSpeech(requestId: String, text: String, voice: String)
    /// Asks the daemon to plan `goal` into an ordered step list via Tinfoil's tool-calling --
    /// this proposes steps for review, it does not execute them. Answered by
    /// `ServerMessage.planReady`/`planFailed`.
    case planTask(requestId: String, goal: String)

    private enum CodingKeys: String, CodingKey {
        case type
        case text
        case contextId = "context_id"
        case requestId = "request_id"
        case selectedOption = "selected_option"
        case event
        case prompt
        case filename
        case dataBase64 = "data_base64"
        case mode
        case imageDataBase64 = "image_data_base64"
        case audioDataBase64 = "audio_data_base64"
        case format
        case voice
        case goal
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
        case clarifyRequest = "clarify_request"
        case processDocument = "process_document"
        case analyzeImage = "analyze_image"
        case transcribeAudio = "transcribe_audio"
        case requestSpeech = "request_speech"
        case planTask = "plan_task"
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
            self = .stop(contextId: try container.decodeIfPresent(String.self, forKey: .contextId))
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
        case .clarifyRequest:
            self = .clarifyRequest(prompt: try container.decode(String.self, forKey: .prompt))
        case .processDocument:
            self = .processDocument(
                requestId: try container.decode(String.self, forKey: .requestId),
                filename: try container.decode(String.self, forKey: .filename),
                dataBase64: try container.decode(String.self, forKey: .dataBase64),
                mode: try container.decode(String.self, forKey: .mode)
            )
        case .analyzeImage:
            self = .analyzeImage(
                requestId: try container.decode(String.self, forKey: .requestId),
                imageDataBase64: try container.decode(String.self, forKey: .imageDataBase64),
                prompt: try container.decode(String.self, forKey: .prompt)
            )
        case .transcribeAudio:
            self = .transcribeAudio(
                requestId: try container.decode(String.self, forKey: .requestId),
                audioDataBase64: try container.decode(String.self, forKey: .audioDataBase64),
                format: try container.decode(String.self, forKey: .format)
            )
        case .requestSpeech:
            self = .requestSpeech(
                requestId: try container.decode(String.self, forKey: .requestId),
                text: try container.decode(String.self, forKey: .text),
                voice: try container.decode(String.self, forKey: .voice)
            )
        case .planTask:
            self = .planTask(
                requestId: try container.decode(String.self, forKey: .requestId),
                goal: try container.decode(String.self, forKey: .goal)
            )
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
        case .stop(let contextId):
            try container.encode(Kind.stop, forKey: .type)
            try container.encodeIfPresent(contextId, forKey: .contextId)
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
        case .clarifyRequest(let prompt):
            try container.encode(Kind.clarifyRequest, forKey: .type)
            try container.encode(prompt, forKey: .prompt)
        case .processDocument(let requestId, let filename, let dataBase64, let mode):
            try container.encode(Kind.processDocument, forKey: .type)
            try container.encode(requestId, forKey: .requestId)
            try container.encode(filename, forKey: .filename)
            try container.encode(dataBase64, forKey: .dataBase64)
            try container.encode(mode, forKey: .mode)
        case .analyzeImage(let requestId, let imageDataBase64, let prompt):
            try container.encode(Kind.analyzeImage, forKey: .type)
            try container.encode(requestId, forKey: .requestId)
            try container.encode(imageDataBase64, forKey: .imageDataBase64)
            try container.encode(prompt, forKey: .prompt)
        case .transcribeAudio(let requestId, let audioDataBase64, let format):
            try container.encode(Kind.transcribeAudio, forKey: .type)
            try container.encode(requestId, forKey: .requestId)
            try container.encode(audioDataBase64, forKey: .audioDataBase64)
            try container.encode(format, forKey: .format)
        case .requestSpeech(let requestId, let text, let voice):
            try container.encode(Kind.requestSpeech, forKey: .type)
            try container.encode(requestId, forKey: .requestId)
            try container.encode(text, forKey: .text)
            try container.encode(voice, forKey: .voice)
        case .planTask(let requestId, let goal):
            try container.encode(Kind.planTask, forKey: .type)
            try container.encode(requestId, forKey: .requestId)
            try container.encode(goal, forKey: .goal)
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
        case .clarifyRequest: return "clarify_request"
        case .processDocument: return "process_document"
        case .analyzeImage: return "analyze_image"
        case .transcribeAudio: return "transcribe_audio"
        case .requestSpeech: return "request_speech"
        case .planTask: return "plan_task"
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
        Self.timeFormatter.string(from: timestamp)
    }

    /// Shared formatter. `DateFormatter()` construction is genuinely expensive
    /// (it builds ICU state), and this is called once per visible log row on
    /// every re-render of the status list, so allocating a fresh one per row
    /// was pure waste in a hot path.
    private static let timeFormatter: DateFormatter = {
        let f = DateFormatter()
        f.dateFormat = "HH:mm:ss"
        return f
    }()
}
