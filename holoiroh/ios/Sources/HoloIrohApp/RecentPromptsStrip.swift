import SwiftData
import SwiftUI

/// Shows up to 12 recently sent prompts in a horizontal strip.
/// Tapping a prompt sends its text to `onPick`.
/// The strip updates from `RecentPrompt` query changes.
/// It does not render when no prompts exist.
/// `MainView` supplies the required `RecentPrompt` model context.
struct RecentPromptsStrip: View {
    @Query(sort: \RecentPrompt.createdAt, order: .reverse) private var prompts: [RecentPrompt]

    /// Receives the selected prompt text.
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
