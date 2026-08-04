import Foundation
import SwiftData

/// Records and retrieves recent prompts from SwiftData.
/// Callers do not access `ModelContext` directly.
///
/// All operations are best effort.
/// If the container is unavailable, writes do nothing and reads return an empty list.
/// SwiftData failures do not block prompt sends.
@MainActor
struct RecentPromptsRepository {
    /// Returns the main-actor context, or `nil` when recent prompts are disabled.
    private var context: ModelContext? { RecentPromptStore.container?.mainContext }

    /// Records a nonempty prompt after trimming surrounding whitespace.
    /// An existing prompt receives a new timestamp instead of a duplicate row.
    /// The repository keeps at most 50 prompts.
    /// The method does not throw.
    func record(_ text: String, appHint: String? = nil) {
        guard let context else { return }
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        context.insert(RecentPrompt(text: trimmed, createdAt: Date(), appHint: appHint))
        try? context.save()
        capHistory(context, keeping: 50)
    }

    /// Returns at most `limit` prompts in descending last-sent order.
    /// Returns an empty list when the container or history is unavailable.
    func recent(limit: Int = 12) -> [RecentPrompt] {
        guard let context else { return [] }
        var descriptor = FetchDescriptor<RecentPrompt>(
            sortBy: [SortDescriptor(\.createdAt, order: .reverse)]
        )
        descriptor.fetchLimit = limit
        return (try? context.fetch(descriptor)) ?? []
    }

    private func capHistory(_ context: ModelContext, keeping max: Int) {
        let descriptor = FetchDescriptor<RecentPrompt>(
            sortBy: [SortDescriptor(\.createdAt, order: .reverse)]
        )
        guard let all = try? context.fetch(descriptor), all.count > max else { return }
        for stale in all[max...] {
            context.delete(stale)
        }
        try? context.save()
    }
}
