import SwiftUI

/// Composes a goal, sends `ClientMessage.planTask`, and shows the resulting step list
/// (`ServerMessage.planReady`) for the user to review. This proposes a plan; it does not run
/// it -- running a step (e.g. "Desktop action: ...") is still a separate, explicit
/// `dispatchPrompt` the user triggers from here, exactly like typing an instruction directly.
struct PlanStepsSheet: View {
    let onSend: (ClientMessage) -> Void
    /// Runs a plan step as a normal desktop-agent prompt (the same path a typed instruction
    /// takes) -- so any existing safeguards (sensitive-app consent, etc.) apply unchanged.
    let onRunStep: (String) -> Void

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
                                if step.hasPrefix("Desktop action:") {
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
                    Text("Planning is sent to Tinfoil's confidential-computing cloud. Nothing runs on the Mac until you tap Run on a step.")
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
                    Button("Close") { dismiss() }
                }
            }
        }
    }

    private func plan() {
        isPlanning = true
        steps = nil
        planError = nil
        onSend(.planTask(requestId: requestId, goal: goal))
    }
}
