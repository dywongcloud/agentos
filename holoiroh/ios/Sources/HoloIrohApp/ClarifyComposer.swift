import Foundation

/// Combines an instruction with answers to the daemon's clarifying questions.
/// The operation is deterministic and has no user-interface dependency.
enum ClarifyComposer {
    /// Appends each nonblank answer to the trimmed instruction.
    /// Returns only the trimmed instruction when no usable answer exists.
    /// A blank question produces a list item that contains only its answer.
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
