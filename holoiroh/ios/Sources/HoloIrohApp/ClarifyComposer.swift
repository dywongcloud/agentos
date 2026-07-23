import Foundation

/// Combines the user's original instruction with the answers they gave to the
/// daemon's clarifying questions into a single, clearer instruction that is sent
/// as the real `ClientMessage.prompt`. Pure + deterministic so it can be
/// exercised by a standalone harness without any UI.
enum ClarifyComposer {
    /// `answers` pairs each clarifying question with the user's chosen (or typed
    /// "something else") answer. Blank answers are dropped. With no usable
    /// answers the original instruction is returned unchanged.
    static func compose(original: String, answers: [(question: String, answer: String)]) -> String {
        let resolved = answers.compactMap { pair -> String? in
            let q = pair.question.trimmingCharacters(in: .whitespacesAndNewlines)
            let a = pair.answer.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !a.isEmpty else { return nil }
            return q.isEmpty ? "- \(a)" : "- \(q) \(a)"
        }
        let base = original.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !resolved.isEmpty else { return base }
        return base + "\n\nClarifications:\n" + resolved.joined(separator: "\n")
    }
}
