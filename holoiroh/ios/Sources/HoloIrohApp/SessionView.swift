import SwiftUI
#if canImport(UIKit)
import UIKit
#elseif canImport(AppKit)
import AppKit
#endif

private extension Color {
    static var sessionSecondaryBackground: Color {
        #if canImport(UIKit)
        Color(uiColor: .secondarySystemBackground)
        #else
        Color(nsColor: .controlBackgroundColor)
        #endif
    }

    static var sessionTertiaryBackground: Color {
        #if canImport(UIKit)
        Color(uiColor: .tertiarySystemBackground)
        #else
        Color(nsColor: .underPageBackgroundColor)
        #endif
    }
}

/// Displays the panel for the current `SessionState`.
///
/// - Maps each Product Requirements Document (PRD) 6.1 state to its content and controls.
/// - Routes each control through `SessionActions`.
/// - Keeps the media stream in `MainView` across state changes.
struct SessionView: View {
    let state: SessionState
    let macName: String
    let macAvailable: Bool
    let inferenceMode: String
    let actions: SessionActions

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            switch state {
            case .idle:
                idlePanel
            case .reviewing(let payload):
                reviewingPanel(payload)
            case .connecting:
                connectingPanel
            case .working(let payload):
                workingPanel(payload)
            case .inputNeeded(let payload):
                inputNeededPanel(payload)
            case .draftReady(let payload):
                draftReadyPanel(payload)
            case .awaitingApproval(let payload):
                awaitingApprovalPanel(payload)
            case .failed(let payload):
                failedPanel(payload)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(16)
        .background(Color.sessionSecondaryBackground)
        .clipShape(RoundedRectangle(cornerRadius: 12))
    }

    // MARK: - Idle

    private var idlePanel: some View {
        VStack(alignment: .leading, spacing: 12) {
            stateHeader
            // Paired-Mac availability, per PRD 6.1 Idle.
            HStack(spacing: 8) {
                Image(systemName: macAvailable ? "checkmark.circle.fill" : "xmark.circle.fill")
                    .foregroundStyle(macAvailable ? Color.green : Color.secondary)
                VStack(alignment: .leading, spacing: 2) {
                    Text(macName)
                        .font(.subheadline.weight(.semibold))
                    Text(macAvailable ? "Available" : "Unavailable")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            Text("Hold the mic or type a request below to start a task.")
                .font(.footnote)
                .foregroundStyle(.secondary)

            // Push-to-talk affordance + the Start control.
            HStack(spacing: 12) {
                Label("Push-to-talk", systemImage: "mic.circle")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                Spacer()
                Button("Start", action: actions.start)
                    .buttonStyle(.borderedProminent)
                    .disabled(!macAvailable)
            }
        }
    }

    // MARK: - Reviewing

    private func reviewingPanel(_ payload: ReviewPayload) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            stateHeader
            field("Transcript", payload.transcript)
            field("Destination", payload.destination)
            field("Dictated text", payload.dictatedText)

            HStack(spacing: 12) {
                Button("Edit", action: actions.edit)
                    .buttonStyle(.bordered)
                Button("Discard", role: .destructive, action: actions.discard)
                    .buttonStyle(.bordered)
                Spacer()
                Button("Send", action: actions.send)
                    .buttonStyle(.borderedProminent)
            }
        }
    }

    // MARK: - Connecting

    private var connectingPanel: some View {
        VStack(alignment: .leading, spacing: 12) {
            stateHeader
            HStack(spacing: 10) {
                ProgressView()
                Text("Establishing the remote-view connection to \(macName)…")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
            HStack {
                Spacer()
                Button("Cancel", role: .destructive, action: actions.cancel)
                    .buttonStyle(.bordered)
            }
        }
    }

    // MARK: - Working

    private func workingPanel(_ payload: WorkingPayload) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            stateHeader
            // Remote View itself is rendered by MainView above this panel
            // (SessionState.showsRemoteView == true here). This panel carries
            // the app/status/last-action/next-action dashboard fields.
            field("App", payload.app)
            field("Status", payload.isPaused ? "Paused — \(payload.status)" : payload.status)
            field("Last action", payload.lastAction)
            field("Next action", payload.nextAction)
            field("Inference", inferenceMode)

            HStack(spacing: 12) {
                Button(payload.isPaused ? "Resume" : "Pause", action: actions.pause)
                    .buttonStyle(.bordered)
                Button("Take control", action: actions.takeControl)
                    .buttonStyle(.bordered)
                Spacer()
                Button("Cancel", role: .destructive, action: actions.cancel)
                    .buttonStyle(.bordered)
            }
        }
    }

    // MARK: - Input needed (P0-14)

    private func inputNeededPanel(_ payload: InputRequestPayload) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            stateHeader
            Text(payload.kind.rawValue)
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(.orange)

            field("What's needed", payload.whatIsNeeded)
            field("Why", payload.why)
            field("Current frame", payload.currentFrame)

            // Response options -> the Choose control (one per option). Empty
            // for free-form kinds (credential/MFA), where the resolution is
            // TakeControl / ResolveLocally instead.
            if !payload.responseOptions.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    Text("RESPONSE OPTIONS")
                        .font(.caption2.weight(.semibold))
                        .foregroundStyle(.tertiary)
                    ForEach(payload.responseOptions, id: \.self) { option in
                        Button {
                            actions.choose(option)
                        } label: {
                            HStack {
                                Text(option)
                                Spacer()
                                Image(systemName: "chevron.right")
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                            .frame(maxWidth: .infinity, alignment: .leading)
                        }
                        .buttonStyle(.bordered)
                    }
                }
            }

            HStack(spacing: 12) {
                Button("Take control", action: actions.takeControl)
                    .buttonStyle(.borderedProminent)
                Button("Resolve locally", action: actions.resolveLocally)
                    .buttonStyle(.bordered)
                Spacer()
                Button("Cancel", role: .destructive, action: actions.cancel)
                    .buttonStyle(.bordered)
            }
        }
    }

    // MARK: - Draft ready

    private func draftReadyPanel(_ payload: DraftPayload) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            stateHeader
            // Remote View (the live target) is rendered by MainView above.
            field("Live target", payload.target)
            field("Draft", payload.draftSummary)
            field("Verification", payload.verification)

            HStack(spacing: 12) {
                Button("Review", action: actions.review)
                    .buttonStyle(.bordered)
                Spacer()
                Button("Cancel", role: .destructive, action: actions.cancel)
                    .buttonStyle(.bordered)
                Button("Request send", action: actions.requestSend)
                    .buttonStyle(.borderedProminent)
            }
        }
    }

    // MARK: - Awaiting approval

    private func awaitingApprovalPanel(_ payload: ApprovalPayload) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            stateHeader
            field("Destination", payload.destination)
            field("Text", payload.text)
            field("Frame", payload.frame)

            // The plain-language commitment the Approve button acts on.
            VStack(alignment: .leading, spacing: 4) {
                Text("COMMITMENT")
                    .font(.caption2.weight(.semibold))
                    .foregroundStyle(.tertiary)
                Text(payload.commitmentDescription)
                    .font(.subheadline.weight(.medium))
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(10)
            .background(Color.sessionTertiaryBackground)
            .clipShape(RoundedRectangle(cornerRadius: 8))

            HStack(spacing: 12) {
                Button("Reject", role: .destructive, action: actions.reject)
                    .buttonStyle(.bordered)
                Spacer()
                Button("Approve", action: actions.approve)
                    .buttonStyle(.borderedProminent)
            }
        }
    }

    // MARK: - Failed

    private func failedPanel(_ payload: FailurePayload) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 8) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .foregroundStyle(.red)
                Text(state.displayName)
                    .font(.headline)
            }
            field("Cause", payload.cause)
            field("Recovery", payload.recovery)

            HStack(spacing: 12) {
                Button("Retry", action: actions.retry)
                    .buttonStyle(.borderedProminent)
                Button("Take control", action: actions.takeControl)
                    .buttonStyle(.bordered)
                Spacer()
                Button("Dismiss", action: actions.dismiss)
                    .buttonStyle(.bordered)
            }
        }
    }

    // MARK: - Shared building blocks

    /// Displays the state name and inference mode for each non-failure panel.
    private var stateHeader: some View {
        HStack {
            Text(state.displayName)
                .font(.headline)
            Spacer()
            Text(inferenceMode)
                .font(.caption2.weight(.semibold))
                .padding(.horizontal, 8)
                .padding(.vertical, 3)
                .background(Color.sessionTertiaryBackground)
                .clipShape(Capsule())
                .foregroundStyle(.secondary)
        }
    }

    /// Displays a read-only field with its label above its value.
    private func field(_ label: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label.uppercased())
                .font(.caption2.weight(.semibold))
                .foregroundStyle(.tertiary)
            Text(value.isEmpty ? "—" : value)
                .font(.subheadline)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// Contains the control callbacks for all PRD 6.1 session panels.
/// Each panel control calls one closure.
/// `choose` passes the selected response option.
struct SessionActions {
    var start: () -> Void = {}
    var edit: () -> Void = {}
    var send: () -> Void = {}
    var discard: () -> Void = {}
    var cancel: () -> Void = {}
    var pause: () -> Void = {}
    var takeControl: () -> Void = {}
    var resolveLocally: () -> Void = {}
    var choose: (String) -> Void = { _ in }
    var review: () -> Void = {}
    var requestSend: () -> Void = {}
    var approve: () -> Void = {}
    var reject: () -> Void = {}
    var retry: () -> Void = {}
    var dismiss: () -> Void = {}
}
