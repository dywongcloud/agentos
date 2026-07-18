import Foundation

/// The eight user-visible session states from Project Aro PRD section 6.1's
/// "states table". This is the *coarse* projection of a task's lifecycle that
/// the iOS app actually shows the user -- deliberately far fewer states than
/// the Mac daemon's fine-grained 30-variant `TaskState`
/// (`holoiroh/mac-daemon/src/task_state.rs`), because the phone's job is to
/// present a small number of decision points with clear controls, not to
/// mirror every internal step of the agent's pipeline.
///
/// Associated values carry the payload each state needs to render its
/// PRD-specified content: `reviewing` carries the transcript/destination/
/// dictated-text under review, `inputNeeded` carries the P0-14 request
/// (what's needed, why, response options), `draftReady`/`awaitingApproval`
/// carry the target + commitment being confirmed, and `failed` carries the
/// actionable cause + recovery guidance.
///
/// ## Mapping: this 8-state iOS `SessionState` projection ⇄ the Rust
/// `task_state.rs` 30-state `TaskState` lifecycle
///
/// The Mac daemon's `TaskState` enum (16 flow + 4 interactive-waiting + 10
/// terminal = 30 variants) is the authoritative, fine-grained lifecycle. This
/// iOS `SessionState` is a coarser 8-bucket *view-model* projection of it:
/// each fine Rust state maps onto exactly one coarse UI state below. When the
/// control channel is eventually wired to carry a real `TaskState` (today it
/// cannot -- see `task_state.rs`'s "Relationship to `holo_bridge::control`"
/// doc: the live `holo serve` A2A stream reports only three coarse outcomes
/// plus free-text, with no per-`TaskState` granularity), this table is the
/// spec for the projection function that would collapse the 30 daemon states
/// into these 8 UI states:
///
/// | iOS `SessionState`      | Rust `TaskState` variants that project onto it                                   |
/// | ----------------------- | -------------------------------------------------------------------------------- |
/// | `.idle`                 | (pre-task: no `TaskState` yet) + `Created`                                       |
/// | `.reviewing`            | (pre-task: transcript captured on-device, not yet a daemon task) + `Queued`     |
/// | `.connecting`           | `Connecting`, `Authenticated`, `RemoteViewStarting`, `NeedsLogin`, `NeedsMfa`   |
/// | `.working`              | `RemoteViewActive`, `PolicyChecking`, `LaunchingApp`, `FindingTarget`,          |
/// |                         | `Navigating`, `TypingDraft`, `Verifying`, `Committing`                          |
/// | `.inputNeeded`          | `NeedsConfirmation`, `SensitiveAccessRequested`                                  |
/// |                         | (the P0-14 interactive-request states surfaced to the user)                     |
/// | `.draftReady`           | `DraftReady`                                                                      |
/// | `.awaitingApproval`     | `AwaitingConfirmation`                                                            |
/// | `.failed`               | All 7 real terminal alternatives + generic failure: `UserCancelled`,            |
/// |                         | `PermissionDenied`, `AmbiguousTarget`, `TargetNotFound`,                         |
/// |                         | `SensitiveAccessRejected`, `AgentTimeout`, `Failed`. (`Completed` is the        |
/// |                         | success terminal -- the UI returns to `.idle` on it rather than showing         |
/// |                         | `.failed`.) The 3 Confidential-Cloud/Tinfoil terminals are unreachable in       |
/// |                         | the alpha build and so never project onto any UI state here.                    |
///
/// Notes on the projection's shape:
/// - `NeedsLogin`/`NeedsMfa` interrupt `Connecting` in the Rust machine, so
///   they project to `.connecting` (the connection is still being brought up),
///   *not* `.inputNeeded` -- `.inputNeeded` is reserved for the P0-14
///   in-task confirmation/sensitive-access requests that occur once the
///   session is already `.working`.
/// - `Completed` deliberately has no `.completed` UI state: PRD 6.1 lists
///   eight states and success returns the dashboard to `.idle` (ready for the
///   next request), with the terminal result surfaced in the log, rather than
///   parking on a dead-end "done" screen.
enum SessionState: Equatable {
    /// Nothing running. The user can push-to-talk / type a new request. The
    /// dashboard shows paired-Mac availability. Control: **Start**.
    case idle

    /// A captured request (voice transcript or typed text) is shown back to
    /// the user for confirmation before it becomes a signed task. Shows the
    /// transcript, the resolved destination, and the dictated text.
    /// Controls: **Edit / Send / Discard**.
    case reviewing(ReviewPayload)

    /// The `iroh` remote-view/control connection to the paired Mac is being
    /// established (covers connect → authenticate → remote-view-starting, and
    /// the login/MFA interrupts that block the connection). Control: **Cancel**.
    case connecting

    /// The agent is actively driving the Mac. Shows the Remote View plus the
    /// active app, status, last completed action, and next intended action.
    /// Controls: **Pause / Cancel / TakeControl**.
    case working(WorkingPayload)

    /// The P0-14 input-request UI: the agent paused the turn to ask the user
    /// a structured question. Shows what's needed, why, the current frame
    /// context, and the response options. Controls: **TakeControl /
    /// ResolveLocally / Choose / Cancel**.
    case inputNeeded(InputRequestPayload)

    /// A draft has passed verification and is ready for the user to inspect
    /// before anything is committed/sent. Shows the live target and the
    /// verification result. Controls: **Review / RequestSend / Cancel**.
    case draftReady(DraftPayload)

    /// The final confirmation gate before a committing action (e.g. clicking
    /// Send). Shows the destination, the text, the current frame, and a
    /// plain-language description of the commitment. Controls: **Approve /
    /// Reject**.
    case awaitingApproval(ApprovalPayload)

    /// The task ended in a failure the user can act on. Shows an actionable
    /// cause and a recovery suggestion. Controls: **Retry / TakeControl /
    /// Dismiss**.
    case failed(FailurePayload)

    /// Short label for the current state, shown in the dashboard header so the
    /// user always knows which of the eight PRD 6.1 states they're in.
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

    /// Whether this state shows the live Remote View. Per the task spec:
    /// Working and DraftReady show the video (the user is watching the agent
    /// act / inspecting a live draft); Idle/Reviewing/Connecting/Failed don't
    /// need it. InputNeeded and AwaitingApproval embed their own frame
    /// snapshot in their payload rather than the full live surface.
    var showsRemoteView: Bool {
        switch self {
        case .working, .draftReady: return true
        default: return false
        }
    }
}

// MARK: - Per-state payloads

/// Payload for `.reviewing`: what the user is confirming before it becomes a
/// signed task. Mirrors PRD 6.1's "transcript / destination / dictated-text".
struct ReviewPayload: Equatable {
    /// The raw transcript as recognized (or typed).
    var transcript: String
    /// The resolved destination the task will target (e.g. "Slack › #design").
    var destination: String
    /// The dictated text/content the task will enter (distinct from the
    /// transcript: the transcript is the *instruction*, this is the *payload*
    /// the instruction produces -- e.g. the message body to be typed).
    var dictatedText: String
}

/// Payload for `.working`: the live task dashboard fields from PRD 6.1 /
/// the dashboard row (app/status/last-action/next-action).
struct WorkingPayload: Equatable {
    /// The app the agent is currently operating in (e.g. "Slack").
    var app: String
    /// A short human-readable status line (e.g. "navigating to #design").
    var status: String
    /// The last action the agent completed (e.g. "clicked the compose field").
    var lastAction: String
    /// The next action the agent intends to take (e.g. "type the message").
    var nextAction: String
    /// Whether the agent is currently paused (Pause toggles this). Paused
    /// still shows the Remote View but the agent takes no further action.
    var isPaused: Bool = false
}

/// The five kinds of P0-14 input request, mirroring the Rust daemon's
/// `input_request` `InputRequestKind` (see `mac-daemon`'s `control_channel.rs`
/// / `input_request_probe.rs`): the agent pauses a turn to ask for one of
/// these. Credentials/MFA codes themselves never travel on the control
/// channel -- only the *request* for them does (see PROTOCOL.md's
/// "Credentials never travel on this channel").
enum InputRequestKind: String, Equatable {
    case credentialNeeded = "Credential needed"
    case mfaNeeded = "Multi-factor code needed"
    case ambiguousChoice = "Ambiguous choice"
    case missingInfo = "Missing information"
    case sensitiveAccess = "Sensitive-access consent"
}

/// Payload for `.inputNeeded`: the P0-14 request UI. Mirrors PRD 6.1's
/// "what's needed / why / current-frame / response-options".
struct InputRequestPayload: Equatable {
    /// Which kind of input the agent is asking for.
    var kind: InputRequestKind
    /// What is needed, in plain language (the "what's needed" field).
    var whatIsNeeded: String
    /// Why the agent needs it (the "why" field) -- gives the user the context
    /// to decide whether to answer, resolve locally, or cancel.
    var why: String
    /// A short description of the current on-screen frame/context the request
    /// is about (the "current-frame" field) -- e.g. "Slack login wall".
    var currentFrame: String
    /// The response options the user can Choose between (the
    /// "response-options" field). Empty for free-form kinds like
    /// credential/MFA, where the resolution is TakeControl / ResolveLocally.
    var responseOptions: [String]
}

/// Payload for `.draftReady`: mirrors PRD 6.1's "live target + verification".
struct DraftPayload: Equatable {
    /// The live target the draft was entered into (e.g. "Slack › #design ›
    /// message composer").
    var target: String
    /// A summary of the drafted content the user is about to review.
    var draftSummary: String
    /// The verification result: what the daemon checked the draft against the
    /// original instruction and found (e.g. "matches request: mentions the
    /// launch date and the design review").
    var verification: String
}

/// Payload for `.awaitingApproval`: mirrors PRD 6.1's "destination / text /
/// frame / commitment description".
struct ApprovalPayload: Equatable {
    /// Where the commitment will land (e.g. "Slack › #design").
    var destination: String
    /// The exact text that will be committed/sent.
    var text: String
    /// A short description of the on-screen frame the commitment acts on.
    var frame: String
    /// A plain-language description of the irreversible commitment being
    /// approved (e.g. "Send this message to #design"). This is the sentence
    /// the Approve button acts on.
    var commitmentDescription: String
}

/// Payload for `.failed`: mirrors PRD 6.1's "actionable cause + recovery".
struct FailurePayload: Equatable {
    /// The actionable cause of the failure, phrased so the user can do
    /// something about it (e.g. "Couldn't find the #design channel -- it may
    /// have been renamed or archived").
    var cause: String
    /// A concrete recovery suggestion (e.g. "Retry, or take control to pick
    /// the channel manually").
    var recovery: String
}
