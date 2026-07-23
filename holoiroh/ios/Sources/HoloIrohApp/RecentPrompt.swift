import Foundation
import SwiftData

/// A prompt the user actually sent to the Mac, kept so the command bar can offer
/// one-tap re-send of recent instructions. GREENFIELD SwiftData (the local-first
/// pass deliberately kept the device-confirmed profile store on raw sqlite and
/// adopts SwiftData only here, where there's no migration risk).
@Model
final class RecentPrompt {
    /// The exact prompt text that was sent.
    @Attribute(.unique) var text: String
    /// When it was last sent (dedup bumps this rather than inserting a copy).
    var createdAt: Date
    /// Optional app the prompt referenced (from OrbEffects' app detection), for a
    /// future grouping/badge affordance. Nil today.
    var appHint: String?

    init(text: String, createdAt: Date = Date(), appHint: String? = nil) {
        self.text = text
        self.createdAt = createdAt
        self.appHint = appHint
    }
}

/// Owns the app-wide SwiftData container for `RecentPrompt`, in its OWN store
/// file separate from the connection-profile sqlite. Deliberately optional and
/// failure-isolated: if the container can't be created, `container` is nil, the
/// recent-prompts feature simply doesn't appear, and NOTHING about pairing or
/// connection (which never touch SwiftData) is affected.
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
