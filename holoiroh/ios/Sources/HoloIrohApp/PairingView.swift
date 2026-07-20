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

    /// Saved connection profiles (sqlite-backed). Selecting one connects
    /// immediately -- its ticket already went through phrase verification
    /// when it was first saved, so re-verifying every reconnect would only
    /// add friction without adding trust.
    @StateObject private var profileStore = ConnectionProfileStore()
    @State private var showSaveNamePrompt = false
    @State private var newProfileName = ""

    private var trimmedTicket: String {
        ticketText.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private var canConnect: Bool {
        !trimmedTicket.isEmpty
    }

    private static let orbAccent = Color(red: 0.30, green: 0.56, blue: 1.0)

    var body: some View {
        VStack(spacing: 20) {
            VStack(spacing: 8) {
                Text("Aro")
                    .font(.system(size: 40, weight: .bold, design: .rounded))
                    .foregroundStyle(
                        LinearGradient(colors: [.white, Self.orbAccent], startPoint: .top, endPoint: .bottom)
                    )
                Text("Scan the QR code the Mac daemon prints, or paste its iroh ticket, to pair.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
            }
            .padding(.top, 32)
            .background(
                RadialGradient(
                    colors: [Self.orbAccent.opacity(0.28), .clear],
                    center: .top,
                    startRadius: 0,
                    endRadius: 260
                )
                .allowsHitTesting(false)
            )

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
                HStack(spacing: 8) {
                    Image(systemName: "exclamationmark.triangle.fill")
                    Text(scanError)
                }
                .font(.caption)
                .foregroundStyle(.red)
                .multilineTextAlignment(.center)
                .padding(10)
                .background(Color.red.opacity(0.12), in: RoundedRectangle(cornerRadius: 10))
                .overlay(
                    RoundedRectangle(cornerRadius: 10)
                        .stroke(Color.red.opacity(0.3), lineWidth: 1)
                )
                .padding(.horizontal)
                .transition(.opacity.combined(with: .move(edge: .top)))
                .animation(.easeOut(duration: 0.2), value: scanError)
            }

            if !profileStore.profiles.isEmpty {
                VStack(alignment: .leading, spacing: 8) {
                    Text("Saved profiles")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    ScrollView {
                        VStack(spacing: 8) {
                            ForEach(profileStore.profiles) { profile in
                                Button {
                                    onConnect(profile.ticket, profile.pin)
                                } label: {
                                    HStack(spacing: 10) {
                                        RoundedRectangle(cornerRadius: 9)
                                            .fill(Self.orbAccent.opacity(0.15))
                                            .frame(width: 36, height: 36)
                                            .overlay(
                                                Image(systemName: "desktopcomputer")
                                                    .font(.system(size: 16, weight: .medium))
                                                    .foregroundStyle(Self.orbAccent)
                                            )
                                        VStack(alignment: .leading, spacing: 2) {
                                            Text(profile.name)
                                                .font(.subheadline.weight(.semibold))
                                                .foregroundStyle(.primary)
                                            Text(profile.phrase)
                                                .font(.system(.caption2, design: .monospaced))
                                                .foregroundStyle(.secondary)
                                        }
                                        Spacer()
                                        Image(systemName: "chevron.right")
                                            .foregroundStyle(.tertiary)
                                    }
                                    .padding(10)
                                    .background(Color(.secondarySystemBackground))
                                    .clipShape(RoundedRectangle(cornerRadius: 12))
                                    .overlay(
                                        RoundedRectangle(cornerRadius: 12)
                                            .stroke(.white.opacity(0.07), lineWidth: 1)
                                    )
                                }
                                .buttonStyle(.plain)
                                .contextMenu {
                                    Button(role: .destructive) {
                                        profileStore.delete(profile)
                                    } label: {
                                        Label("Delete profile", systemImage: "trash")
                                    }
                                }
                            }
                        }
                    }
                    .frame(maxHeight: 190)
                }
                .padding(.horizontal)
            }

            Spacer()

            HStack(spacing: 12) {
                Button {
                    // Prefill with the ticket's phrase as a recognizable
                    // default name; the alert lets the user replace it.
                    newProfileName = PairingPhrase.phrase(for: trimmedTicket)
                    showSaveNamePrompt = true
                } label: {
                    Label("Save", systemImage: "square.and.arrow.down")
                }
                .buttonStyle(.bordered)
                .disabled(!canConnect)

                Button {
                    // Do NOT connect yet — require phrase verification first.
                    showVerification = true
                } label: {
                    Text("Connect")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .buttonBorderShape(.roundedRectangle(radius: 14))
                .tint(Self.orbAccent)
                .disabled(!canConnect)
            }
            .padding(.horizontal)
            .padding(.bottom, 32)
        }
        .preferredColorScheme(.dark)
        .background(Color.black.ignoresSafeArea())
        .alert("Save profile", isPresented: $showSaveNamePrompt) {
            TextField("Profile name", text: $newProfileName)
            Button("Save") {
                profileStore.save(name: newProfileName, ticket: trimmedTicket, pin: pinText.trimmingCharacters(in: .whitespacesAndNewlines))
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Saves this ticket and PIN so you can reconnect with one tap.")
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
