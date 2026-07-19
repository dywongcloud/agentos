import SwiftUI

/// Pairing screen: the user scans (or pastes) the iroh ticket printed by
/// `mac-daemon` on startup, confirms the short verification phrase matches
/// what the Mac shows, then connects.
///
/// ## Flow (Project Aro PRD P0-2)
/// 1. **Scan or paste the ticket.** "Scan QR" opens a live camera scanner
///    (`QRScannerSheet` → `QRScannerView`, AVFoundation) whose decoded
///    string is run through `PairingTicket.extract` and auto-filled into the
///    ticket field. Pasting the ticket text is the equivalent manual path.
/// 2. **Verify the short phrase.** Tapping Connect does *not* connect
///    immediately: it opens `PairingVerificationView`, which shows a short
///    phrase deterministically derived from the ticket (`PairingPhrase`).
///    The user confirms it matches the phrase the Mac is displaying next to
///    its QR. Only that explicit confirmation calls `onConnect`. A
///    substituted QR/ticket yields a different phrase, so a
///    man-in-the-middle is caught here.
///
/// `onConnect` is still handed the raw ticket string and it's the caller's
/// (`ContentView`'s) responsibility to decide what "connect" means — that
/// contract is unchanged; the verification gate is entirely local to this
/// screen, so `ContentView`'s navigation wiring did not have to change.
struct PairingView: View {
    /// Called only after the user has confirmed the verification phrase
    /// matches the Mac's, with the trimmed ticket and the pairing PIN the
    /// daemon displayed (empty if the daemon runs with `--no-pin-auth` or
    /// the device is already allowlisted -- see PROTOCOL.md's PIN handshake).
    let onConnect: (_ ticket: String, _ pin: String) -> Void

    @State private var ticketText: String = ""
    @State private var pinText: String = ""
    @State private var showScanner = false
    @State private var showVerification = false
    @State private var scanError: String?

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
                Text("Scan the QR code the Mac daemon prints, or paste its iroh ticket, to pair.")
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

            VStack(alignment: .leading, spacing: 8) {
                Text("Pairing PIN")
                    .font(.caption)
                    .foregroundStyle(.secondary)

                // The short PIN the daemon prints beside its QR (PROTOCOL.md's
                // pre-session PIN handshake). Optional: an already-allowlisted
                // device (or a daemon run with `--no-pin-auth`) needs none.
                TextField("PIN shown by the Mac (optional)", text: $pinText)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(.body, design: .monospaced))
                    .keyboardType(.numberPad)
                    .accessibilityLabel("Pairing PIN field")
            }
            .padding(.horizontal)

            Button {
                scanError = nil
                showScanner = true
            } label: {
                Label("Scan QR", systemImage: "qrcode.viewfinder")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.bordered)
            .padding(.horizontal)

            if let scanError {
                Text(scanError)
                    .font(.caption)
                    .foregroundStyle(.red)
                    .multilineTextAlignment(.center)
                    .padding(.horizontal)
            }

            Spacer()

            Button {
                // Do NOT connect yet — require phrase verification first.
                showVerification = true
            } label: {
                Text("Connect")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .disabled(!canConnect)
            .padding(.horizontal)
            .padding(.bottom, 32)
        }
        // Scanner: decode -> extract the ticket -> auto-fill the field.
        .sheet(isPresented: $showScanner) {
            QRScannerSheet { scanned in
                if let ticket = PairingTicket.extract(from: scanned) {
                    ticketText = ticket
                    scanError = nil
                } else {
                    scanError = "That QR code didn't contain an iroh ticket. Paste the ticket text instead."
                }
            }
        }
        // Verification gate: confirm the short phrase, then connect.
        .sheet(isPresented: $showVerification) {
            PairingVerificationView(
                ticket: trimmedTicket,
                onConfirmed: {
                    showVerification = false
                    onConnect(trimmedTicket, pinText.trimmingCharacters(in: .whitespacesAndNewlines))
                },
                onRejected: {
                    showVerification = false
                }
            )
        }
    }
}

#Preview("Pairing - empty") {
    PairingView(onConnect: { _, _ in })
}

#Preview("Pairing - filled") {
    PairingView(onConnect: { _, _ in })
        .onAppear {}
}
