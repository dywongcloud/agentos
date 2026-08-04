import Foundation

/// Defines the app's eight user-visible session states from Project Aro PRD section 6.1.
///
/// The daemon uses a finer task lifecycle.
/// This enum presents only the states that require distinct app content or controls.
/// Completed tasks return to `idle`.
/// Other terminal results use `failed`.
enum SessionState: Equatable {
    /// Indicates that no task is running.
    /// The user can start a new request.
    case idle

    /// Presents a captured request for confirmation before submission.
    /// The user can edit, send, or discard it.
    case reviewing(ReviewPayload)

    /// Indicates that the app is establishing and authenticating the Mac connection.
    /// Login and multi-factor authentication interruptions remain in this state.
    case connecting

    /// Indicates that the agent is actively driving the Mac.
    /// The app shows live video and task controls.
    case working(WorkingPayload)

    /// Presents a structured question after the daemon pauses the task.
    /// The user can choose an option, take control, resolve locally, or cancel.
    case inputNeeded(InputRequestPayload)

    /// Presents a verified draft before any external commitment.
    /// The user can review, request sending, or cancel.
    case draftReady(DraftPayload)

    /// Presents the final confirmation before an external commitment.
    /// The user can approve or reject the action.
    case awaitingApproval(ApprovalPayload)

    /// Presents an actionable task failure.
    /// The user can retry, take control, or dismiss it.
    case failed(FailurePayload)

    /// Returns the session-state label shown in the dashboard header.
    var displayName: String {
        switch self {
        case .idle: return "Idle"
        case .reviewing: return "Reviewing"
        case .connecting: return "Connecting"
        case .working: return "Working"
        case .inputNeeded: return "Input needed"
        case .draftReady: return "Draft ready"
        case .awaitingApproval: return "Awaiting approval"
        case .failed: return "Failed"
        }
    }

    /// Reports whether the state shows live video.
    /// `working`, `draftReady`, and `failed` return `true`.
    /// Other states return `false`.
    var showsRemoteView: Bool {
        switch self {
        case .working, .draftReady, .failed: return true
        default: return false
        }
    }
}

// MARK: - Per-state payloads

/// Contains the request that the user reviews before submission.
struct ReviewPayload: Equatable {
    /// Contains the recognized or typed instruction.
    var transcript: String
    /// Identifies the resolved task destination.
    var destination: String
    /// Contains the content that the task will enter.
    /// This value is separate from the instruction transcript.
    var dictatedText: String
}

/// Contains the fields shown by the live task dashboard.
struct WorkingPayload: Equatable {
    /// Identifies the app that the agent is operating.
    var app: String
    /// Describes the task's current status.
    var status: String
    /// Describes the agent's most recently completed action.
    var lastAction: String
    /// Describes the agent's next intended action.
    var nextAction: String
    /// Reports whether the task is paused.
    /// Paused tasks keep live video visible and perform no further agent action.
    var isPaused: Bool = false
}

/// Identifies the five supported structured input-request categories.
/// Credentials and multi-factor codes never travel through the control channel.
enum InputRequestKind: String, Equatable {
    case credentialNeeded = "Credential needed"
    case mfaNeeded = "Multi-factor code needed"
    case ambiguousChoice = "Ambiguous choice"
    case missingInfo = "Missing information"
    case sensitiveAccess = "Sensitive-access consent"
}

/// Contains one structured input request shown by `inputNeeded`.
struct InputRequestPayload: Equatable {
    /// Identifies the requested input category.
    var kind: InputRequestKind
    /// Describes the input that the agent needs.
    var whatIsNeeded: String
    /// Explains why the agent needs the input.
    var why: String
    /// Describes the on-screen context for the request.
    var currentFrame: String
    /// Lists the choices that the user can select.
    /// Credential and multi-factor requests can leave this list empty.
    var responseOptions: [String]
}

/// Contains the target and verification details for a ready draft.
struct DraftPayload: Equatable {
    /// Identifies the target that contains the draft.
    var target: String
    /// Summarizes the drafted content for review.
    var draftSummary: String
    /// Describes how the daemon verified the draft against the instruction.
    var verification: String
}

/// Contains the details required for final commitment approval.
struct ApprovalPayload: Equatable {
    /// Identifies where the commitment will occur.
    var destination: String
    /// Contains the exact text for the commitment.
    var text: String
    /// Describes the on-screen context for the commitment.
    var frame: String
    /// Describes the irreversible action that approval authorizes.
    var commitmentDescription: String
}

/// Contains an actionable failure and its recovery guidance.
struct FailurePayload: Equatable {
    /// Describes the failure cause in actionable terms.
    var cause: String
    /// Describes a concrete recovery action.
    var recovery: String
}
