import SwiftData
import SwiftUI

/// A compact, horizontally-scrolling strip of recently-sent prompts shown just
/// above the command bar. Tapping one refills the prompt field so the user can
/// re-send (or tweak first). `@Query`-driven, so it live-updates the instant a
/// new prompt is recorded; renders nothing when there's no history yet.
///
/// Only ever placed inside a view tree that has the `RecentPrompt` container in
/// its environment (MainView gates it on `RecentPromptStore.container != nil`),
/// so `@Query` always has a context to read.
struct RecentPromptsStrip: View {
    @Query(sort: \RecentPrompt.createdAt, order: .reverse) private var prompts: [RecentPrompt]

    /// Called with the tapped prompt's text (the caller refills the field).
    let onPick: (String) -> Void

    var body: some View {
        if !prompts.isEmpty {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 8) {
                    ForEach(prompts.prefix(12)) { prompt in
                        Button {
                            onPick(prompt.text)
                        } label: {
                            Text(prompt.text)
                                .font(.caption)
                                .lineLimit(1)
                                .padding(.horizontal, 12)
                                .padding(.vertical, 7)
                                .background(.ultraThinMaterial, in: Capsule())
                                .overlay(
                                    Capsule().strokeBorder(Color.white.opacity(0.10), lineWidth: 1)
                                )
                                .foregroundStyle(.white.opacity(0.85))
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding(.horizontal, 2)
            }
            .frame(height: 34)
        }
    }
}
