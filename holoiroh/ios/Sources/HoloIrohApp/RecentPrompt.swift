import Foundation
import SwiftData

/// Stores a prompt that the user sent to the daemon.
/// The command bar uses stored prompts for one-tap resend.
@Model
final class RecentPrompt {
    /// Stores the exact prompt text that the user sent.
    @Attribute(.unique) var text: String
    /// Records when the user last sent the prompt.
    var createdAt: Date
    /// Identifies an app detected in the prompt, when available.
    var appHint: String?

    init(text: String, createdAt: Date = Date(), appHint: String? = nil) {
        self.text = text
        self.createdAt = createdAt
        self.appHint = appHint
    }
}

/// Owns the app-wide SwiftData container for recent prompts.
/// The container uses a store separate from connection profiles.
/// If initialization fails, recent prompts are disabled.
/// This failure does not affect pairing or connections.
enum RecentPromptStore {
    static let container: ModelContainer? = {
        do {
            let dir = FileManager.default
                .urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
                .appendingPathComponent("HoloIroh", isDirectory: true)
            try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            let config = ModelConfiguration(url: dir.appendingPathComponent("RecentPrompts.store"))
            return try ModelContainer(for: RecentPrompt.self, configurations: config)
        } catch {
            NSLog("RecentPromptStore: ModelContainer init failed (\(error)) -- recent prompts disabled; pairing/connection unaffected")
            return nil
        }
    }()
}
