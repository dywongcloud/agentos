import SwiftUI

/// Pairing screen: the user pastes (or, eventually, scans) the iroh
/// ticket printed by `mac-daemon` on startup, then taps Connect to move
/// to `MainView`.
///
/// Networking is not wired up yet -- `onConnect` is handed the raw
/// pasted ticket string and it's the caller's (currently `ContentView`'s)
/// responsibility to decide what "connect" means. This view only owns
/// ticket-text state and the two button actions described in the task:
/// paste + a "Scan QR" placeholder.
struct PairingView: View {
    /// Called when the user taps Connect with a non-empty ticket.
    let onConnect: (String) -> Void

    @State private var ticketText: String = ""
    @State private var showScanPlaceholderAlert = false

    private var trimmedTicket: String {
        ticketText.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private var canConnect: Bool {
        !trimmedTicket.isEmpty
    }

    var body: some View {
        VStack(spacing: 20) {
            VStack(spacing: 8) {
                Text("HoloIroh")
                    .font(.largeTitle)
                    .fontWeight(.bold)
                Text("Paste the iroh ticket printed by the Mac daemon, or scan its QR code, to pair.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
            }
            .padding(.top, 32)

            VStack(alignment: .leading, spacing: 8) {
                Text("Iroh ticket")
                    .font(.caption)
                    .foregroundStyle(.secondary)

                // A ticket is a long opaque token, not a single short
                // word, so a multi-line editor (rather than a single-line
                // TextField) avoids the pasted value scrolling off-screen
                // horizontally. Capped height + internal scrolling keeps
                // very long tickets from pushing the Connect button off
                // the bottom of the layout.
                TextEditor(text: $ticketText)
                    .font(.system(.footnote, design: .monospaced))
                    .frame(height: 120)
                    .scrollContentBackground(.hidden)
                    .padding(8)
                    .background(Color(.secondarySystemBackground))
                    .clipShape(RoundedRectangle(cornerRadius: 8))
                    .overlay(
                        RoundedRectangle(cornerRadius: 8)
                            .strokeBorder(Color.secondary.opacity(0.3))
                    )
                    .overlay(alignment: .topLeading) {
                        if ticketText.isEmpty {
                            Text("iroh-live:…")
                                .font(.system(.footnote, design: .monospaced))
                                .foregroundStyle(.tertiary)
                                .padding(.horizontal, 12)
                                .padding(.vertical, 16)
                                .allowsHitTesting(false)
                        }
                    }
                    .accessibilityLabel("Iroh ticket text field")
            }
            .padding(.horizontal)

            Button {
                // Placeholder: real QR scanning (AVFoundation capture
                // session + code detection) is a later task. For now this
                // surfaces that the control exists without pretending to
                // scan anything.
                showScanPlaceholderAlert = true
            } label: {
                Label("Scan QR", systemImage: "qrcode.viewfinder")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.bordered)
            .padding(.horizontal)
            .alert("Scan QR", isPresented: $showScanPlaceholderAlert) {
                Button("OK", role: .cancel) {}
            } message: {
                Text("QR scanning isn't implemented yet -- paste the ticket text above instead.")
            }

            Spacer()

            Button {
                onConnect(trimmedTicket)
            } label: {
                Text("Connect")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .disabled(!canConnect)
            .padding(.horizontal)
            .padding(.bottom, 32)
        }
    }
}

#Preview("Pairing - empty") {
    PairingView(onConnect: { _ in })
}

#Preview("Pairing - filled") {
    PairingView(onConnect: { _ in })
        .onAppear {}
}
