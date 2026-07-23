import SwiftUI

/// The clarifying-questions panel shown ABOVE the command bar when the daemon
/// returns questions for an ambiguous instruction. Each question is answered by
/// picking one of its concrete options or the final "Something else…" option
/// (which reveals a free-text field). The Continue button — enabled once every
/// question is answered — proceeds with the clarified task.
struct ClarifyPanel: View {
    let questions: [ClarifyingQuestion]
    let onCancel: () -> Void
    let onContinue: ([(question: String, answer: String)]) -> Void

    static let somethingElseLabel = "Something else…"

    /// questionId -> the chosen option label (may be `somethingElseLabel`).
    @State private var choice: [String: String] = [:]
    /// questionId -> the typed free-text answer (only when "Something else…" is chosen).
    @State private var customText: [String: String] = [:]

    private func resolvedAnswer(_ q: ClarifyingQuestion) -> String? {
        guard let picked = choice[q.id] else { return nil }
        if picked == Self.somethingElseLabel {
            let typed = (customText[q.id] ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            return typed.isEmpty ? nil : typed
        }
        return picked
    }

    private var allAnswered: Bool {
        questions.allSatisfy { resolvedAnswer($0) != nil }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack {
                Label("A few quick questions", systemImage: "sparkles")
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(Color.aroAccentBright)
                Spacer()
                Button(action: onCancel) {
                    Image(systemName: "xmark")
                        .font(.system(size: 12, weight: .bold))
                        .foregroundStyle(.white.opacity(0.5))
                        .frame(width: 26, height: 26)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Dismiss clarifying questions")
            }

            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    ForEach(questions) { question in
                        questionBlock(question)
                    }
                }
            }
            .frame(maxHeight: 320)

            Button {
                onContinue(questions.map { (question: $0.question, answer: resolvedAnswer($0) ?? "") })
            } label: {
                Label("Continue", systemImage: "arrow.right")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(AroPrimaryButtonStyle(enabled: allAnswered))
            .disabled(!allAnswered)
        }
        .padding(16)
        .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 20, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 20, style: .continuous)
                .strokeBorder(Color.white.opacity(0.12), lineWidth: 1)
        )
        .shadow(color: .black.opacity(0.4), radius: 18, y: 6)
    }

    private func questionBlock(_ question: ClarifyingQuestion) -> some View {
        VStack(alignment: .leading, spacing: 9) {
            Text(question.question)
                .font(.subheadline)
                .foregroundStyle(.white)
                .fixedSize(horizontal: false, vertical: true)

            LazyVGrid(
                columns: [GridItem(.adaptive(minimum: 96), spacing: 8)],
                alignment: .leading,
                spacing: 8
            ) {
                ForEach(question.options + [Self.somethingElseLabel], id: \.self) { option in
                    optionChip(question, option)
                }
            }

            if choice[question.id] == Self.somethingElseLabel {
                TextField("Type your answer", text: customBinding(question.id))
                    .textFieldStyle(.plain)
                    .font(.callout)
                    .foregroundStyle(.white)
                    .tint(Color.aroAccentBright)
                    .padding(10)
                    .background(Color.white.opacity(0.06), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
                    .overlay(
                        RoundedRectangle(cornerRadius: 10, style: .continuous)
                            .strokeBorder(Color.white.opacity(0.12), lineWidth: 1)
                    )
            }
        }
    }

    private func optionChip(_ question: ClarifyingQuestion, _ option: String) -> some View {
        let isSelected = choice[question.id] == option
        return Button {
            choice[question.id] = option
        } label: {
            Text(option)
                .font(.caption)
                .lineLimit(2)
                .multilineTextAlignment(.center)
                .frame(maxWidth: .infinity)
                .padding(.horizontal, 10)
                .padding(.vertical, 8)
                .background(
                    isSelected ? Color.aroAccent.opacity(0.35) : Color.white.opacity(0.06),
                    in: Capsule()
                )
                .overlay(
                    Capsule().strokeBorder(
                        isSelected ? Color.aroAccentBright : Color.white.opacity(0.12),
                        lineWidth: 1
                    )
                )
                .foregroundStyle(isSelected ? .white : .white.opacity(0.82))
        }
        .buttonStyle(.plain)
    }

    private func customBinding(_ id: String) -> Binding<String> {
        Binding(get: { customText[id] ?? "" }, set: { customText[id] = $0 })
    }
}
