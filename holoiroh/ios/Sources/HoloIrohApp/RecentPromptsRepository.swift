import Foundation
import SwiftData

/// Local-first repository over the `RecentPrompt` SwiftData store (mirrors
/// `ConnectionProfileRepository`'s role for the profile store): the one place
/// that records sent prompts and fetches them back, so call sites never touch a
/// `ModelContext` directly.
///
/// Every method is best-effort and failure-isolated: with no container
/// (`RecentPromptStore.container == nil`) each call is a silent no-op / empty
/// result, so a SwiftData failure can never block or break a prompt send.
@MainActor
struct RecentPromptsRepository {
    /// The store's main-actor context, or nil when the container failed to
    /// init (feature disabled). Resolved here rather than in a default init
    /// argument, which -- being nonisolated -- can't touch `mainContext`.
    private var context: ModelContext? { RecentPromptStore.container?.mainContext }

    /// Records `text` as a recent prompt. Dedups on the model's unique `text`
    /// (a repeat re-sends bump `createdAt` to the top rather than duplicating),
    /// then caps the store so history can't grow without bound. Never throws.
    func record(_ text: String, appHint: String? = nil) {
        guard let context else { return }
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        context.insert(RecentPrompt(text: trimmed, createdAt: Date(), appHint: appHint))
        try? context.save()
        capHistory(context, keeping: 50)
    }

    /// The most-recent-first prompts, capped to `limit`. Empty when there's no
    /// container or nothing has been sent yet.
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
