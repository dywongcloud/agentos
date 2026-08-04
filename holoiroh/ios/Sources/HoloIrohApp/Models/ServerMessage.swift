import Foundation
#if canImport(HoloIrohMicrophoneCapture)
import HoloIrohMicrophoneCapture
#endif

/// Contains one daemon-generated clarifying question and its suggested answers.
/// The app adds a free-text choice after these options.
struct ClarifyingQuestion: Codable, Equatable, Identifiable {
    let question: String
    let options: [String]

    var id: String { question }
}

enum ApprovalRisk: String, Codable, Equatable {
    case low
    case medium
    case high
    case critical
}

struct ApprovalEffect: Codable, Equatable {
    let app: String
    let target: String
    let material: String

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        app = try container.decode(String.self, forKey: .app)
        target = try container.decode(String.self, forKey: .target)
        material = try container.decode(String.self, forKey: .material)
        guard Self.isValidField(app, maximumBytes: 128),
              Self.isValidField(target, maximumBytes: 512),
              Self.isValidField(material, maximumBytes: 1_024)
        else {
            throw DecodingError.dataCorruptedError(
                forKey: .app,
                in: container,
                debugDescription: "approval effect fields are empty or exceed their byte limits"
            )
        }
    }

    private static func isValidField(_ value: String, maximumBytes: Int) -> Bool {
        !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && value.utf8.count <= maximumBytes
            && !value.unicodeScalars.contains(where: { $0.value < 0x20 && $0 != "\n" && $0 != "\t" })
    }
}

struct ApprovalRequest: Codable, Equatable, Identifiable {
    let approvalId: String
    let actionId: String
    let proposalDigest: String
    let runId: String
    let taskId: String
    let risk: ApprovalRisk
    let effect: ApprovalEffect
    let beforeStateDigest: String
    let expiresAt: UInt64

    var id: String { approvalId }

    private enum CodingKeys: String, CodingKey {
        case approvalId = "approval_id"
        case actionId = "action_id"
        case proposalDigest = "proposal_digest"
        case runId = "run_id"
        case taskId = "task_id"
        case risk
        case effect
        case beforeStateDigest = "before_state_digest"
        case expiresAt = "expires_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        approvalId = try container.decode(String.self, forKey: .approvalId)
        actionId = try container.decode(String.self, forKey: .actionId)
        proposalDigest = try container.decode(String.self, forKey: .proposalDigest)
        runId = try container.decode(String.self, forKey: .runId)
        taskId = try container.decode(String.self, forKey: .taskId)
        risk = try container.decode(ApprovalRisk.self, forKey: .risk)
        effect = try container.decode(ApprovalEffect.self, forKey: .effect)
        beforeStateDigest = try container.decode(String.self, forKey: .beforeStateDigest)
        expiresAt = try container.decode(UInt64.self, forKey: .expiresAt)
        let identifiers = [approvalId, actionId, runId, taskId]
        guard identifiers.allSatisfy({ Self.isBoundedIdentifier($0) }),
              Self.isLowercaseSHA256(proposalDigest),
              Self.isLowercaseSHA256(beforeStateDigest),
              expiresAt > UInt64(Date().timeIntervalSince1970 * 1_000)
        else {
            throw DecodingError.dataCorruptedError(
                forKey: .approvalId,
                in: container,
                debugDescription: "approval bindings, digests, or expiry are invalid"
            )
        }
    }

    private static func isBoundedIdentifier(_ value: String) -> Bool {
        !value.isEmpty && value.utf8.count <= 128
            && value.unicodeScalars.allSatisfy { $0.value >= 0x21 && $0.value <= 0x7e }
    }

    private static func isLowercaseSHA256(_ value: String) -> Bool {
        value.utf8.count == 64
            && value.utf8.allSatisfy { ($0 >= 48 && $0 <= 57) || ($0 >= 97 && $0 <= 102) }
    }
}

struct TinfoilMeasurement: Codable, Equatable {
    let type: String
    let registers: [String]
}

struct TinfoilGroundTruth: Codable, Equatable {
    let digest: String
    let tlsPublicKey: String?
    let hpkePublicKey: String?
    let codeMeasurement: TinfoilMeasurement
    let enclaveMeasurement: TinfoilMeasurement
    let codeFingerprint: String
    let enclaveFingerprint: String

    private enum CodingKeys: String, CodingKey {
        case digest
        case tlsPublicKey = "tls_public_key"
        case hpkePublicKey = "hpke_public_key"
        case codeMeasurement = "code_measurement"
        case enclaveMeasurement = "enclave_measurement"
        case codeFingerprint = "code_fingerprint"
        case enclaveFingerprint = "enclave_fingerprint"
    }
}

struct TinfoilVerification: Codable, Equatable {
    let host: String
    let groundTruth: TinfoilGroundTruth
}

struct TypedPrompt: Codable, Equatable {
    let goalId: String
    let instruction: String

    private enum CodingKeys: String, CodingKey {
        case goalId = "goal_id"
        case instruction
    }
}

struct TypedPlan: Codable, Equatable {
    let planId: String
    let goalDigest: String
    let steps: [PlannedStep]

    private enum CodingKeys: String, CodingKey {
        case planId = "plan_id"
        case goalDigest = "goal_digest"
        case steps
    }
}

struct TypedActionProposal: Codable, Equatable {
    let goalId: String
    let intentDigest: String
    let runId: String
    let taskId: String
    let actionId: String
    let observation: TypedObservation
    let target: TypedTarget
    let action: TypedDesktopAction
    let proposalDigest: String

    private enum CodingKeys: String, CodingKey {
        case goalId = "goal_id"
        case intentDigest = "intent_digest"
        case runId = "run_id"
        case taskId = "task_id"
        case actionId = "action_id"
        case observation, target, action
        case proposalDigest = "proposal_digest"
    }
}

struct TypedObservation: Codable, Equatable {
    let observationId: String
    let beforeStateDigest: String
    private enum CodingKeys: String, CodingKey {
        case observationId = "observation_id"
        case beforeStateDigest = "before_state_digest"
    }
}

struct TypedTarget: Codable, Equatable {
    let bundleId: String
    let windowId: String
    let elementId: String
    let expectedRole: String
    let expectedTitleDigest: String
    let expectedValueDigest: String?
    let sensitive: Bool
    let credential: Bool
    let resolved: Bool
    private enum CodingKeys: String, CodingKey {
        case bundleId = "bundle_id"
        case windowId = "window_id"
        case elementId = "element_id"
        case expectedRole = "expected_role"
        case expectedTitleDigest = "expected_title_digest"
        case expectedValueDigest = "expected_value_digest"
        case sensitive, credential, resolved
    }
}

enum TypedDesktopAction: Codable, Equatable {
    case observe
    case navigate(TypedNavigationAction)
    case focus
    case draftText(String)
    case commit(TypedCommitAction)

    private enum CodingKeys: String, CodingKey { case type, navigation, text, commit }
    private enum Kind: String, Codable { case observe, navigate, focus, draftText = "draft_text", commit }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        switch try container.decode(Kind.self, forKey: .type) {
        case .observe: self = .observe
        case .navigate: self = .navigate(try container.decode(TypedNavigationAction.self, forKey: .navigation))
        case .focus: self = .focus
        case .draftText: self = .draftText(try container.decode(String.self, forKey: .text))
        case .commit: self = .commit(try container.decode(TypedCommitAction.self, forKey: .commit))
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .observe: try container.encode(Kind.observe, forKey: .type)
        case .navigate(let navigation):
            try container.encode(Kind.navigate, forKey: .type)
            try container.encode(navigation, forKey: .navigation)
        case .focus: try container.encode(Kind.focus, forKey: .type)
        case .draftText(let text):
            try container.encode(Kind.draftText, forKey: .type)
            try container.encode(text, forKey: .text)
        case .commit(let commit):
            try container.encode(Kind.commit, forKey: .type)
            try container.encode(commit, forKey: .commit)
        }
    }
}

enum TypedNavigationAction: Codable, Equatable {
    case semanticActivate
    case coordinateActivate(x: Int32, y: Int32)
    case scroll(horizontal: Int32, vertical: Int32)

    private enum CodingKeys: String, CodingKey { case type, x, y, horizontal, vertical }
    private enum Kind: String, Codable { case semanticActivate = "semantic_activate", coordinateActivate = "coordinate_activate", scroll }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        switch try container.decode(Kind.self, forKey: .type) {
        case .semanticActivate: self = .semanticActivate
        case .coordinateActivate: self = .coordinateActivate(x: try container.decode(Int32.self, forKey: .x), y: try container.decode(Int32.self, forKey: .y))
        case .scroll: self = .scroll(horizontal: try container.decode(Int32.self, forKey: .horizontal), vertical: try container.decode(Int32.self, forKey: .vertical))
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .semanticActivate: try container.encode(Kind.semanticActivate, forKey: .type)
        case .coordinateActivate(let x, let y):
            try container.encode(Kind.coordinateActivate, forKey: .type); try container.encode(x, forKey: .x); try container.encode(y, forKey: .y)
        case .scroll(let horizontal, let vertical):
            try container.encode(Kind.scroll, forKey: .type); try container.encode(horizontal, forKey: .horizontal); try container.encode(vertical, forKey: .vertical)
        }
    }
}

enum TypedCommitAction: String, Codable, Equatable {
    case sendMessage = "send_message", submitForm = "submit_form", publish, purchase, transferFunds = "transfer_funds", deleteItem = "delete_item"
}

enum PlannedStep: Codable, Equatable {
    case action(TypedActionProposal)
    case complete
    private enum CodingKeys: String, CodingKey { case kind, proposal }
    private enum Kind: String, Codable { case action, complete }
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        switch try container.decode(Kind.self, forKey: .kind) {
        case .action: self = .action(try container.decode(TypedActionProposal.self, forKey: .proposal))
        case .complete: self = .complete
        }
    }
    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .action(let proposal): try container.encode(Kind.action, forKey: .kind); try container.encode(proposal, forKey: .proposal)
        case .complete: try container.encode(Kind.complete, forKey: .kind)
        }
    }
}

enum PlannerRunStatus: String, Codable, Equatable { case planning, ready, executing, completed, failed, canceled }

struct PlannerReceipt: Codable, Equatable {
    let planId: String
    let goalDigest: String
    let actionId: String
    let proposalDigest: String
    let status: PlannerRunStatus
    private enum CodingKeys: String, CodingKey {
        case planId = "plan_id", goalDigest = "goal_digest", actionId = "action_id", proposalDigest = "proposal_digest", status
    }
}

/// Mirrors daemon-to-app messages from `PROTOCOL.md`.
/// The `type` field selects each wire case.
/// Decoding fails when the wire type is unknown or required data is absent.
enum ServerMessage: Codable, Equatable {
    case ack(text: String?)
    case status(text: String?, executionMode: String?, capabilities: [String]?)
    case error(text: String?)
    case taskProgress(text: String?)
    /// Reports a terminal task result.
    /// `status` contains the daemon's snake-case completion status.
    case taskDone(status: String, text: String?)
    /// Reports a task that remained active across reconnection.
    /// `paused` identifies a parked task.
    /// `queued` counts waiting prompts.
    case taskActive(paused: Bool, queued: Int)
    /// Reports authentication rejection before the daemon closes the connection.
    case authRejected(text: String?)
    /// Carries the daemon's current ticket after authentication.
    /// The app uses it to refresh a stale saved ticket.
    case currentTicket(ticket: String)
    case tinfoilVerification(TinfoilVerification)
    /// Carries questions generated for `ClientMessage.clarifyRequest`.
    /// An empty list means the original instruction needs no clarification.
    case clarifyQuestions(questions: [ClarifyingQuestion])
    /// Carries a structured input request from the daemon.
    /// Reply with `ClientMessage.inputResponse`.
    /// The reply must echo `requestId` and select one listed option.
    case inputRequest(
        requestId: String,
        kind: String,
        context: String,
        responseOptions: [String],
        expiresAt: UInt64
    )
    case approvalRequest(ApprovalRequest)
    /// Reports whether the focused Mac field is a secure input.
    /// The daemon sends this case when the state changes.
    /// The app uses it to explain ScreenCaptureKit redaction.
    case secureInputState(active: Bool)
    /// Returns markdown for `ClientMessage.processDocument`.
    case documentProcessed(requestId: String, markdown: String)
    /// Returns a document-processing error.
    case documentProcessFailed(requestId: String, error: String)
    /// Returns text for `ClientMessage.analyzeImage`.
    case imageAnalyzed(requestId: String, text: String)
    /// Returns an image-analysis error.
    case imageAnalysisFailed(requestId: String, error: String)
    /// Returns text for `ClientMessage.transcribeAudio`.
    case audioTranscribed(requestId: String, text: String)
    /// Returns an audio-transcription error.
    case audioTranscriptionFailed(requestId: String, error: String)
    /// Returns WAV data for `ClientMessage.requestSpeech`.
    /// `audioDataBase64` contains the encoded WAV bytes.
    case speechReady(requestId: String, audioDataBase64: String)
    /// Returns a speech-synthesis error.
    case speechFailed(requestId: String, error: String)
    case typedPlanReady(requestId: String, plan: TypedPlan)
    case plannerStatus(requestId: String, status: PlannerRunStatus, text: String?)
    case plannerReceipt(requestId: String, receipt: PlannerReceipt)
    /// Returns an ordered step list for `ClientMessage.planTask`.
    /// The list is a plan and does not execute actions.
    case planReady(requestId: String, steps: [String])
    /// Returns a task-planning error.
    case planFailed(requestId: String, error: String)

    /// Returns a status without execution metadata.
    static func status(text: String?) -> Self {
        .status(text: text, executionMode: nil, capabilities: nil)
    }

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
        case host
        case groundTruth = "ground_truth"
        case questions
        case active
        case markdown
        case error
        case audioDataBase64 = "audio_data_base64"
        case plan
        case receipt
        case steps
        case executionMode = "execution_mode"
        case capabilities
        case approvalId = "approval_id"
        case actionId = "action_id"
        case proposalDigest = "proposal_digest"
        case runId = "run_id"
        case taskId = "task_id"
        case effect
        case risk
        case beforeStateDigest = "before_state_digest"
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
        case tinfoilVerification = "tinfoil_verification"
        case clarifyQuestions = "clarify_questions"
        case inputRequest = "input_request"
        case approvalRequest = "approval_request"
        case secureInputState = "secure_input_state"
        case documentProcessed = "document_processed"
        case documentProcessFailed = "document_process_failed"
        case imageAnalyzed = "image_analyzed"
        case imageAnalysisFailed = "image_analysis_failed"
        case audioTranscribed = "audio_transcribed"
        case audioTranscriptionFailed = "audio_transcription_failed"
        case speechReady = "speech_ready"
        case speechFailed = "speech_failed"
        case typedPlanReady = "typed_plan_ready"
        case plannerStatus = "planner_status"
        case plannerReceipt = "planner_receipt"
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
            self = .status(
                text: try container.decodeIfPresent(String.self, forKey: .text),
                executionMode: try container.decodeIfPresent(String.self, forKey: .executionMode),
                capabilities: try container.decodeIfPresent([String].self, forKey: .capabilities)
            )
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
        case .tinfoilVerification:
            self = .tinfoilVerification(TinfoilVerification(
                host: try container.decode(String.self, forKey: .host),
                groundTruth: try container.decode(TinfoilGroundTruth.self, forKey: .groundTruth)
            ))
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
        case .approvalRequest:
            self = .approvalRequest(try ApprovalRequest(from: decoder))
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
        case .typedPlanReady:
            self = .typedPlanReady(
                requestId: try container.decode(String.self, forKey: .requestId),
                plan: try container.decode(TypedPlan.self, forKey: .plan)
            )
        case .plannerStatus:
            self = .plannerStatus(
                requestId: try container.decode(String.self, forKey: .requestId),
                status: try container.decode(PlannerRunStatus.self, forKey: .status),
                text: try container.decodeIfPresent(String.self, forKey: .text)
            )
        case .plannerReceipt:
            self = .plannerReceipt(
                requestId: try container.decode(String.self, forKey: .requestId),
                receipt: try container.decode(PlannerReceipt.self, forKey: .receipt)
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
        case .status(let text, let executionMode, let capabilities):
            try container.encode(Kind.status, forKey: .type)
            try container.encodeIfPresent(text, forKey: .text)
            try container.encodeIfPresent(executionMode, forKey: .executionMode)
            try container.encodeIfPresent(capabilities, forKey: .capabilities)
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
        case .tinfoilVerification(let verification):
            try container.encode(Kind.tinfoilVerification, forKey: .type)
            try container.encode(verification.host, forKey: .host)
            try container.encode(verification.groundTruth, forKey: .groundTruth)
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
        case .approvalRequest(let request):
            try container.encode(Kind.approvalRequest, forKey: .type)
            try container.encode(request.approvalId, forKey: .approvalId)
            try container.encode(request.actionId, forKey: .actionId)
            try container.encode(request.proposalDigest, forKey: .proposalDigest)
            try container.encode(request.runId, forKey: .runId)
            try container.encode(request.taskId, forKey: .taskId)
            try container.encode(request.risk, forKey: .risk)
            try container.encode(request.effect, forKey: .effect)
            try container.encode(request.beforeStateDigest, forKey: .beforeStateDigest)
            try container.encode(request.expiresAt, forKey: .expiresAt)
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
        case .typedPlanReady(let requestId, let plan):
            try container.encode(Kind.typedPlanReady, forKey: .type)
            try container.encode(requestId, forKey: .requestId)
            try container.encode(plan, forKey: .plan)
        case .plannerStatus(let requestId, let status, let text):
            try container.encode(Kind.plannerStatus, forKey: .type)
            try container.encode(requestId, forKey: .requestId)
            try container.encode(status, forKey: .status)
            try container.encodeIfPresent(text, forKey: .text)
        case .plannerReceipt(let requestId, let receipt):
            try container.encode(Kind.plannerReceipt, forKey: .type)
            try container.encode(requestId, forKey: .requestId)
            try container.encode(receipt, forKey: .receipt)
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

    /// Returns the request identifier for a Tinfoil response.
    /// Returns `nil` for other messages.
    var tinfoilRequestId: String? {
        switch self {
        case .documentProcessed(let requestId, _),
             .documentProcessFailed(let requestId, _),
             .imageAnalyzed(let requestId, _),
             .imageAnalysisFailed(let requestId, _),
             .audioTranscribed(let requestId, _),
             .audioTranscriptionFailed(let requestId, _),
             .speechReady(let requestId, _),
             .speechFailed(let requestId, _),
             .typedPlanReady(let requestId, _),
             .plannerStatus(let requestId, _, _),
             .plannerReceipt(let requestId, _),
             .planReady(let requestId, _),
             .planFailed(let requestId, _):
            return requestId
        default:
            return nil
        }
    }

    /// Returns text for the status panel.
    /// Cases without text use a label derived from their wire type.
    var displayText: String {
        switch self {
        case .ack(let text): return text ?? "ack"
        case .status(let text, _, _): return text ?? "status"
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
        case .tinfoilVerification(let verification):
            return "Tinfoil attestation verified for \(verification.host)"
        case .clarifyQuestions(let questions):
            return questions.isEmpty ? "no clarification needed" : "\(questions.count) clarifying question(s)"
        case .inputRequest(_, _, let context, _, _): return context
        case .approvalRequest(let request): return request.effect.material
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
        case .typedPlanReady(_, let plan): return "\(plan.steps.count) typed step plan ready"
        case .plannerStatus(_, let status, let text): return text ?? status.rawValue
        case .plannerReceipt(_, let receipt): return "\(receipt.status.rawValue): \(receipt.actionId)"
        case .planReady(_, let steps): return "\(steps.count) step plan ready"
        case .planFailed(_, let error): return error
        }
    }

    /// Returns the short message-kind label used by the log list.
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
        case .tinfoilVerification: return "VERIFY"
        case .clarifyQuestions: return "CLARIFY"
        case .inputRequest: return "INPUT"
        case .approvalRequest: return "APPROVAL"
        case .secureInputState: return "LOCK"
        case .documentProcessed, .documentProcessFailed: return "DOCUMENT"
        case .imageAnalyzed, .imageAnalysisFailed: return "IMAGE"
        case .audioTranscribed, .audioTranscriptionFailed: return "TRANSCRIBE"
        case .speechReady, .speechFailed: return "SPEECH"
        case .typedPlanReady, .plannerStatus, .plannerReceipt: return "TYPED"
        case .planReady, .planFailed: return "PLAN"
        }
    }
}

enum ApprovalDecision: String, Codable, Equatable {
    case approve
    case deny
    case cancel
}

/// Mirrors app-to-daemon messages from `PROTOCOL.md`.
enum ClientMessage: Codable, Equatable {
    case typedPrompt(TypedPrompt)
    case prompt(text: String)
    case voiceTranscript(text: String)
    /// Requests task cancellation.
    /// A nil `contextId` cancels the running turn and drains the queue.
    /// A nonnil value limits cancellation to one A2A context.
    case stop(contextId: String?)
    /// Parks the running task for a later resume.
    case pause
    /// Continues a parked task on the same backend session.
    case resume
    /// Replaces running or queued work while preserving task session history.
    case redirect(text: String)
    /// Answers a structured input request.
    /// `requestId` must match the request.
    /// `selectedOption` must copy one offered option.
    /// This message never carries free text or credentials.
    case inputResponse(requestId: String, selectedOption: String)
    case approvalResponse(
        approvalId: String,
        actionId: String,
        proposalDigest: String,
        decision: ApprovalDecision
    )
    /// Sends one normalized remote-control action from the live video surface.
    case remoteControl(RemoteControlEvent)
    /// Requests clarifying questions before the daemon runs an instruction.
    case clarifyRequest(prompt: String)
    /// Requests Tinfoil document conversion.
    /// The daemon replies with a document success or failure case.
    case processDocument(requestId: String, filename: String, dataBase64: String, mode: String)
    /// Requests Tinfoil image analysis.
    /// The app redacts the image before upload.
    /// The daemon replies with an image success or failure case.
    case analyzeImage(requestId: String, imageDataBase64: String, prompt: String)
    /// Requests optional Tinfoil audio transcription.
    ///
    /// `HoloIrohMicrophoneCapture` creates the opaque capture from this device's microphone.
    /// The app cannot wrap arbitrary bytes, system audio, or speaker output in this case.
    /// The default voice transcript path remains on-device.
    case transcribeAudio(requestId: String, capture: CapturedMicrophoneAudio)
    /// Requests Tinfoil speech synthesis.
    /// The daemon replies with a speech success or failure case.
    case requestSpeech(requestId: String, text: String, voice: String)
    /// Requests a Tinfoil-generated step plan for review.
    /// This message does not execute the plan.
    case planTask(requestId: String, goal: String)

    private enum CodingKeys: String, CodingKey {
        case type
        case text
        case contextId = "context_id"
        case requestId = "request_id"
        case selectedOption = "selected_option"
        case approvalId = "approval_id"
        case actionId = "action_id"
        case proposalDigest = "proposal_digest"
        case decision
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
        case typedPrompt = "typed_prompt"
        case prompt
        case voiceTranscript = "voice_transcript"
        case stop
        case pause
        case resume
        case redirect
        case inputResponse = "input_response"
        case approvalResponse = "approval_response"
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
        case .typedPrompt:
            self = .typedPrompt(try container.decode(TypedPrompt.self, forKey: .prompt))
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
        case .approvalResponse:
            self = .approvalResponse(
                approvalId: try container.decode(String.self, forKey: .approvalId),
                actionId: try container.decode(String.self, forKey: .actionId),
                proposalDigest: try container.decode(String.self, forKey: .proposalDigest),
                decision: try container.decode(ApprovalDecision.self, forKey: .decision)
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
            throw DecodingError.dataCorruptedError(
                forKey: .type,
                in: container,
                debugDescription: "transcribe_audio is outbound-only; construct it from CapturedMicrophoneAudio"
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
        case .typedPrompt(let prompt):
            try container.encode(Kind.typedPrompt, forKey: .type)
            try container.encode(prompt, forKey: .prompt)
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
        case .approvalResponse(let approvalId, let actionId, let proposalDigest, let decision):
            try container.encode(Kind.approvalResponse, forKey: .type)
            try container.encode(approvalId, forKey: .approvalId)
            try container.encode(actionId, forKey: .actionId)
            try container.encode(proposalDigest, forKey: .proposalDigest)
            try container.encode(decision, forKey: .decision)
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
        case .transcribeAudio(let requestId, let capture):
            try container.encode(Kind.transcribeAudio, forKey: .type)
            try container.encode(requestId, forKey: .requestId)
            try container.encode(capture.audioDataBase64, forKey: .audioDataBase64)
            try container.encode(capture.format, forKey: .format)
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

    var requiresAutonomousHolo: Bool {
        switch self {
        case .typedPrompt:
            return false
        case .prompt, .voiceTranscript, .redirect:
            return true
        default:
            return false
        }
    }

    /// Returns the exact snake-case wire type label.
    var wireKindLabel: String {
        switch self {
        case .typedPrompt: return "typed_prompt"
        case .prompt: return "prompt"
        case .voiceTranscript: return "voice_transcript"
        case .stop: return "stop"
        case .pause: return "pause"
        case .resume: return "resume"
        case .redirect: return "redirect"
        case .inputResponse: return "input_response"
        case .approvalResponse: return "approval_response"
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

/// Adds a stable identifier and timestamp to a server message.
/// The log uses these values for identity and chronological display.
struct LogEntry: Identifiable, Equatable {
    let id: UUID
    let timestamp: Date
    let message: ServerMessage

    init(id: UUID = UUID(), timestamp: Date = Date(), message: ServerMessage) {
        self.id = id
        self.timestamp = timestamp
        self.message = message
    }

    /// Returns the timestamp in `HH:mm:ss` format.
    var formattedTime: String {
        Self.timeFormatter.string(from: timestamp)
    }

    /// Shares one date formatter across all log entries.
    private static let timeFormatter: DateFormatter = {
        let f = DateFormatter()
        f.dateFormat = "HH:mm:ss"
        return f
    }()
}
