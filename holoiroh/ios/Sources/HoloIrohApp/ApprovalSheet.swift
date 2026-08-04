import SwiftUI

struct ApprovalSheet: View {
    let request: ApprovalRequest
    let onDecision: (ApprovalDecision) -> Void

    private var expiry: Date {
        Date(timeIntervalSince1970: TimeInterval(request.expiresAt) / 1_000)
    }

    var body: some View {
        NavigationStack {
            TimelineView(.periodic(from: .now, by: 1)) { timeline in
                let expired = timeline.date >= expiry
                Form {
                    Section("Requested effect") {
                        row("App", request.effect.app)
                        row("Target", request.effect.target)
                        row("Material", request.effect.material)
                    }
                    Section("Safety") {
                        row("Risk", request.risk.rawValue.capitalized)
                        row("Expires", expiry.formatted(date: .abbreviated, time: .standard))
                        if expired {
                            Label("This request has expired. Approval is unavailable.", systemImage: "clock.badge.exclamationmark")
                                .foregroundStyle(.red)
                        }
                    }
                    Section {
                        Button("Approve") { onDecision(.approve) }
                            .disabled(expired)
                        Button("Deny", role: .destructive) { onDecision(.deny) }
                        Button("Cancel", role: .cancel) { onDecision(.cancel) }
                    }
                }
            }
            .navigationTitle("Action approval")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { onDecision(.cancel) }
                }
            }
        }
    }

    private func row(_ label: String, _ value: String) -> some View {
        LabeledContent(label) {
            Text(value)
                .multilineTextAlignment(.trailing)
                .textSelection(.enabled)
        }
    }
}
