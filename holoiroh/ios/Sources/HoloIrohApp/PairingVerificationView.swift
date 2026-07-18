import SwiftUI

/// The short-phrase mutual-verification step (Project Aro PRD P0-2).
///
/// After a ticket is scanned or pasted, this screen shows the verification
/// phrase derived from that ticket (`PairingPhrase`) and asks the user to
/// confirm it matches the phrase the **Mac** is displaying next to its QR.
/// Only an explicit "matches" advances to connecting; a "doesn't match"
/// aborts, because a mismatch means the ticket the phone holds is not the
/// ticket the Mac published — the signature of a man-in-the-middle who
/// substituted the QR.
///
/// This is deliberately a *blocking gate*: `onConfirmed` is the only path
/// forward, and the caller must not connect until it fires. The phrase is
/// derived purely and deterministically, so the same ticket always shows the
/// same phrase here and (once the daemon side is wired — see
/// `holoiroh/ios/PAIRING_PHRASE.md`) on the Mac.
struct PairingVerificationView: View {
    /// The canonical ticket being verified. The phrase is derived from this.
    let ticket: String

    /// Called when the user confirms the phrase matches the Mac's — the only
    /// path that should lead to an actual connection.
    let onConfirmed: () -> Void

    /// Called when the user reports a mismatch, or cancels — pairing must be
    /// abandoned (do not connect).
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
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { onRejected() }
                }
            }
        }
    }

    /// The derived words rendered as a wrapping row of chips.
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
                    .background(Color(.secondarySystemBackground))
                    .clipShape(RoundedRectangle(cornerRadius: 10))
                    .accessibilityLabel("Pairing word: \(word)")
            }
        }
        .frame(maxWidth: .infinity)
    }
}

/// A minimal wrapping horizontal layout (iOS 16+ `Layout`), so the phrase
/// chips wrap to a new line on narrow widths rather than clipping. Kept tiny
/// and local — the app has no general flow-layout need beyond this.
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
