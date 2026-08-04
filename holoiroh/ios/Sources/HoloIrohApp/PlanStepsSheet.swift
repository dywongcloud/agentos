import SwiftUI

/// Requests a task plan and shows the returned steps.
/// A plan request does not run its steps.
/// The user must select Run for each desktop action.
struct PlanStepsSheet: View {
    let onSend: (ClientMessage) -> Void
    /// Runs the selected step through the standard prompt path.
    /// Existing safeguards apply to the step.
    let onRunStep: (String) -> Void
    let autonomousExecutionPermitted: Bool

    @Environment(\.dismiss) private var dismiss

    @State private var goal: String = ""
    @State private var isPlanning = false

    @Binding var steps: [String]?
    @Binding var planError: String?
    let requestId: String

    var body: some View {
        NavigationStack {
            Form {
                Section("Goal") {
                    TextField("What do you want Aro to do?", text: $goal, axis: .vertical)
                    Button(isPlanning ? "Planning…" : "Plan") {
                        plan()
                    }
                    .disabled(isPlanning || goal.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }

                if let steps, !steps.isEmpty {
                    Section("Proposed steps") {
                        ForEach(Array(steps.enumerated()), id: \.offset) { index, step in
                            HStack(alignment: .top) {
                                Text("\(index + 1).")
                                    .foregroundStyle(.secondary)
                                Text(step)
                                Spacer()
                                if step.hasPrefix("Desktop action:"), autonomousExecutionPermitted {
                                    Button("Run") {
                                        onRunStep(String(step.dropFirst("Desktop action:".count)).trimmingCharacters(in: .whitespaces))
                                        dismiss()
                                    }
                                    .buttonStyle(.borderedProminent)
                                    .controlSize(.small)
                                }
                            }
                        }
                    }
                }
                if let planError {
                    Section("Error") {
                        Text(planError).foregroundStyle(.red)
                    }
                }

                Section {
                    if !autonomousExecutionPermitted {
                        Label("Restricted mode — plan steps cannot run autonomously.", systemImage: "hand.raised.fill")
                            .font(.caption.weight(.semibold))
                    }
                    Text("Planning is sent to Tinfoil's confidential-computing cloud. Nothing runs on the Mac until you tap Run on a step. Inspect this connection's cryptographic proof in Diagnostics → Verification Center.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .navigationTitle("Plan a Task")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") {
                        isPlanning = false
                        dismiss()
                    }
                }
            }
            .onChange(of: steps) { _, steps in
                if steps != nil { isPlanning = false }
            }
            .onChange(of: planError) { _, error in
                if error != nil { isPlanning = false }
            }
            .onDisappear { isPlanning = false }
        }
    }

    private func plan() {
        isPlanning = true
        steps = nil
        planError = nil
        onSend(.planTask(requestId: requestId, goal: goal))
    }
}
