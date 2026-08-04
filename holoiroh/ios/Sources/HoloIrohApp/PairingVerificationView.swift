import SwiftUI
#if canImport(UIKit)
import UIKit
#elseif canImport(AppKit)
import AppKit
#endif

private extension Color {
    static var pairingSecondaryBackground: Color {
        #if canImport(UIKit)
        Color(uiColor: .secondarySystemBackground)
        #else
        Color(nsColor: .controlBackgroundColor)
        #endif
    }
}

/// Blocks connection until the user verifies the ticket phrase.
///
/// The app derives the phrase from the ticket.
/// The daemon displays the same phrase beside its pairing code.
/// A match calls `onConfirmed`.
/// A mismatch or cancellation calls `onRejected`.
struct PairingVerificationView: View {
    /// Provides the ticket used to derive the verification phrase.
    let ticket: String

    /// Runs only after the user confirms that both phrases match.
    let onConfirmed: () -> Void

    /// Runs after a mismatch or cancellation. The caller must abandon pairing.
    let onRejected: () -> Void

    private var words: [String] {
        PairingPhrase.words(for: ticket)
    }

    var body: some View {
        NavigationStack {
            VStack(spacing: 24) {
                VStack(spacing: 8) {
                    Image(systemName: "checkmark.shield")
                        .font(.system(size: 44))
                        .foregroundStyle(.tint)
                        .padding(.top, 12)
                    Text("Confirm the pairing phrase")
                        .font(.title2.weight(.semibold))
                        .multilineTextAlignment(.center)
                    Text("Your Mac is showing a short phrase next to its QR code. Check that it matches the phrase below — if it doesn't, someone may have tampered with the code you scanned.")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                        .padding(.horizontal)
                }

                // The phrase, one word per chip, big and legible so it is
                // easy to read aloud and compare against the Mac.
                phraseChips
                    .padding(.horizontal)

                Spacer()

                VStack(spacing: 12) {
                    Button {
                        onConfirmed()
                    } label: {
                        Text("It matches — connect")
                            .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.borderedProminent)

                    Button(role: .destructive) {
                        onRejected()
                    } label: {
                        Text("It doesn't match")
                            .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.bordered)
                }
                .padding(.horizontal)
                .padding(.bottom, 24)
            }
            .navigationTitle("Verify pairing")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { onRejected() }
                }
            }
        }
    }

    /// Displays the derived phrase as a wrapping row of word chips.
    private var phraseChips: some View {
        // A simple wrapping layout: for 4 short words a single HStack is
        // enough on any iPhone width, but allow it to wrap defensively.
        FlowRow(spacing: 10) {
            ForEach(Array(words.enumerated()), id: \.offset) { _, word in
                Text(word)
                    .font(.system(.title3, design: .rounded).weight(.semibold))
                    .monospaced()
                    .padding(.horizontal, 14)
                    .padding(.vertical, 10)
                    .background(Color.pairingSecondaryBackground)
                    .clipShape(RoundedRectangle(cornerRadius: 10))
                    .accessibilityLabel("Pairing word: \(word)")
            }
        }
        .frame(maxWidth: .infinity)
    }
}

/// Wraps phrase chips on narrow layouts. The app uses this layout only for pairing verification.
private struct FlowRow: Layout {
    var spacing: CGFloat = 8

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let maxWidth = proposal.width ?? .infinity
        var rowWidth: CGFloat = 0
        var rowHeight: CGFloat = 0
        var totalHeight: CGFloat = 0
        var totalWidth: CGFloat = 0

        for subview in subviews {
            let size = subview.sizeThatFits(.unspecified)
            if rowWidth > 0, rowWidth + spacing + size.width > maxWidth {
                totalWidth = max(totalWidth, rowWidth)
                totalHeight += rowHeight + spacing
                rowWidth = size.width
                rowHeight = size.height
            } else {
                rowWidth += (rowWidth > 0 ? spacing : 0) + size.width
                rowHeight = max(rowHeight, size.height)
            }
        }
        totalWidth = max(totalWidth, rowWidth)
        totalHeight += rowHeight
        return CGSize(width: totalWidth, height: totalHeight)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var x = bounds.minX
        var y = bounds.minY
        var rowHeight: CGFloat = 0

        for subview in subviews {
            let size = subview.sizeThatFits(.unspecified)
            if x > bounds.minX, x + size.width > bounds.maxX {
                x = bounds.minX
                y += rowHeight + spacing
                rowHeight = 0
            }
            subview.place(at: CGPoint(x: x, y: y), anchor: .topLeading, proposal: ProposedViewSize(size))
            x += size.width + spacing
            rowHeight = max(rowHeight, size.height)
        }
    }
}

#Preview("Verification") {
    PairingVerificationView(
        ticket: "iroh-live:TleiXllmGyIDcEOXtF-AIExJQnPFPlZuzkXmR6OVWNwDAQDAqAFM09EDAQDAqEAB09EDAQDAqP8K09ED/holoiroh",
        onConfirmed: {},
        onRejected: {}
    )
}
