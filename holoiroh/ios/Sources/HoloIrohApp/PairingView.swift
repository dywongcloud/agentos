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

    /// Which pairing field owns the keyboard. Neither field can dismiss the
    /// keyboard on its own -- the ticket editor is a multi-line `TextEditor`
    /// (return inserts a newline, never submits) and the PIN field uses the
    /// `.numberPad` keyboard (which has no return key at all) -- so a
    /// keyboard-toolbar Done button plus tap-outside-to-dismiss, both driven
    /// by clearing this focus, are the ONLY ways off the keyboard here.
    private enum Field: Hashable {
        case ticket
        case pin
    }
    @FocusState private var focusedField: Field?

    /// Saved connection profiles (sqlite-backed). Selecting one connects
    /// immediately -- its ticket already went through phrase verification
    /// when it was first saved, so re-verifying every reconnect would only
    /// add friction without adding trust.
    /// The app-wide store, injected by `HoloIrohApp` (seeded at launch). Read
    /// via `@EnvironmentObject` -- NOT a per-view `@StateObject` -- so the
    /// default profile is guaranteed present regardless of when/whether this
    /// view's own lifecycle would have created + seeded a store.
    @EnvironmentObject private var profileStore: ConnectionProfileStore
    @State private var showSaveNamePrompt = false
    @State private var newProfileName = ""

    private var trimmedTicket: String {
        ticketText.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private var canConnect: Bool {
        !trimmedTicket.isEmpty
    }

    var body: some View {
        // A ScrollView, not a fixed VStack: the header + saved profiles + two
        // glass input cards + scan + actions exceed a phone's usable height, and
        // in a fixed VStack that overflow pushed the saved-profiles section off
        // the bottom -- the "I open the app and don't see the saved profile"
        // symptom. Scrolling guarantees every section, especially the seeded
        // "Dev Mac" reconnect, is always reachable.
        ScrollView {
            VStack(spacing: 18) {
                header
                    .padding(.top, 36)

                // Saved profiles FIRST when present: opening the app surfaces
                // the current-daemon "Dev Mac" one-tap reconnect immediately,
                // above the manual scan/paste inputs.
                if !profileStore.profiles.isEmpty {
                    savedProfilesSection
                        .padding(.horizontal, 20)
                }

                inputCard
                    .padding(.horizontal, 20)

                scanButton
                    .padding(.horizontal, 20)

                if let scanError {
                    scanErrorBanner(scanError)
                        .padding(.horizontal, 20)
                        .transition(.opacity.combined(with: .move(edge: .top)))
                        .animation(.easeOut(duration: 0.2), value: scanError)
                }

                actionBar
                    .padding(.horizontal, 20)
                    .padding(.top, 4)
                    .padding(.bottom, 30)
            }
        }
        .scrollDismissesKeyboard(.interactively)
        .preferredColorScheme(.dark)
        // Backdrop doubles as tap-outside-to-dismiss for the keyboard: the
        // tap only ever clears field focus, and buttons/fields hit-test
        // first, so nothing else on the screen changes behavior.
        .background(
            PairingBackdrop()
                .contentShape(Rectangle())
                .onTapGesture { focusedField = nil }
        )
        // The standard iOS affordance for keyboards with no dismiss key
        // (multi-line editor + number pad): the shared Done bar riding above
        // the keyboard, clearing focus for whichever field owns it. One
        // merged keyboard toolbar is shown for both fields.
        .keyboardDoneToolbar { focusedField = nil }
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
        .onAppear(perform: autoFocusForWitnessIfNeeded)
    }

    // MARK: - Sections

    /// Brand header: the glowing orb mark, the wordmark, and a concise line
    /// that names what you're pairing with.
    private var header: some View {
        VStack(spacing: 14) {
            AroOrbMark(diameter: 58)
            VStack(spacing: 6) {
                AroWordmark(size: 46)
                    // Discreet fallback for opening the hidden diagnostics
                    // screen (alongside the shake gesture) -- posts the same
                    // notification ContentView's `.onShake` listens for.
                    .onLongPressGesture(minimumDuration: 1.0) {
                        NotificationCenter.default.post(name: UIDevice.deviceDidShakeNotification, object: nil)
                    }
                Text("Pair with the Mac running the Aro daemon")
                    .font(.footnote)
                    .foregroundStyle(.white.opacity(0.55))
                    .multilineTextAlignment(.center)
            }
        }
    }

    /// The two frosted input cards: the iroh ticket editor and the pairing PIN.
    private var inputCard: some View {
        VStack(spacing: 14) {
            AroCard {
                VStack(alignment: .leading, spacing: 10) {
                    AroFieldLabel(title: "Iroh ticket", systemImage: "ticket")
                    ticketEditor
                }
            }
            AroCard {
                VStack(alignment: .leading, spacing: 10) {
                    AroFieldLabel(title: "Pairing PIN", systemImage: "lock")
                    pinField
                }
            }
        }
    }

    /// A ticket is a long opaque token, not a single short word, so a
    /// multi-line editor (rather than a single-line TextField) avoids the
    /// pasted value scrolling off-screen horizontally. Capped height + internal
    /// scrolling keeps very long tickets from pushing the actions off-screen.
    private var ticketEditor: some View {
        TextEditor(text: $ticketText)
            .font(.system(.footnote, design: .monospaced))
            .foregroundStyle(.white)
            .tint(Color.aroAccentBright)
            .focused($focusedField, equals: .ticket)
            .frame(height: 104)
            .scrollContentBackground(.hidden)
            .padding(10)
            .background(
                Color.white.opacity(0.05),
                in: RoundedRectangle(cornerRadius: 10, style: .continuous)
            )
            .overlay(
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .strokeBorder(Color.white.opacity(0.09), lineWidth: 1)
            )
            .overlay(alignment: .topLeading) {
                if ticketText.isEmpty {
                    Text("iroh-live:…")
                        .font(.system(.footnote, design: .monospaced))
                        .foregroundStyle(.white.opacity(0.25))
                        .padding(.horizontal, 14)
                        .padding(.vertical, 18)
                        .allowsHitTesting(false)
                }
            }
            .accessibilityLabel("Iroh ticket text field")
    }

    /// The short PIN the daemon prints beside its QR (PROTOCOL.md's pre-session
    /// PIN handshake). Optional: an already-allowlisted device (or a daemon run
    /// with `--no-pin-auth`) needs none.
    private var pinField: some View {
        TextField("PIN shown by the Mac (optional)", text: $pinText)
            .font(.system(.body, design: .monospaced))
            .foregroundStyle(.white)
            .tint(Color.aroAccentBright)
            .keyboardType(.numberPad)
            .focused($focusedField, equals: .pin)
            .padding(12)
            .background(
                Color.white.opacity(0.05),
                in: RoundedRectangle(cornerRadius: 10, style: .continuous)
            )
            .overlay(
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .strokeBorder(Color.white.opacity(0.09), lineWidth: 1)
            )
            .accessibilityLabel("Pairing PIN field")
    }

    /// Secondary glass button that opens the live QR scanner.
    private var scanButton: some View {
        Button {
            scanError = nil
            showScanner = true
        } label: {
            Label("Scan QR code", systemImage: "qrcode.viewfinder")
                .frame(maxWidth: .infinity)
        }
        .buttonStyle(AroSecondaryButtonStyle())
    }

    /// The inline scan-failure banner.
    private func scanErrorBanner(_ message: String) -> some View {
        HStack(spacing: 8) {
            Image(systemName: "exclamationmark.triangle.fill")
            Text(message)
        }
        .font(.caption)
        .foregroundStyle(Color(red: 1.0, green: 0.5, blue: 0.45))
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(12)
        .background(Color.red.opacity(0.12), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(Color.red.opacity(0.3), lineWidth: 1)
        )
    }

    /// The one-tap reconnect list -- sleek glass cards. Selecting one connects
    /// immediately (it was phrase-verified when first saved).
    private var savedProfilesSection: some View {
        VStack(alignment: .leading, spacing: 10) {
            AroFieldLabel(title: "Saved profiles", systemImage: "bookmark")
            VStack(spacing: 10) {
                    ForEach(profileStore.profiles) { profile in
                        Button {
                            onConnect(profile.ticket, profile.pin)
                        } label: {
                            HStack(spacing: 12) {
                                RoundedRectangle(cornerRadius: 10, style: .continuous)
                                    .fill(Color.aroAccent.opacity(0.18))
                                    .frame(width: 40, height: 40)
                                    .overlay(
                                        Image(systemName: "desktopcomputer")
                                            .font(.system(size: 17, weight: .medium))
                                            .foregroundStyle(Color.aroAccentBright)
                                    )
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(profile.name)
                                        .font(.subheadline.weight(.semibold))
                                        .foregroundStyle(.white)
                                    Text(profile.phrase)
                                        .font(.system(.caption2, design: .monospaced))
                                        .foregroundStyle(.white.opacity(0.5))
                                }
                                Spacer()
                                Image(systemName: "arrow.right.circle.fill")
                                    .font(.system(size: 20))
                                    .foregroundStyle(Color.aroAccent.opacity(0.85))
                            }
                            .padding(12)
                            .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
                            .overlay(
                                RoundedRectangle(cornerRadius: 14, style: .continuous)
                                    .strokeBorder(Color.white.opacity(0.10), lineWidth: 1)
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
    }

    /// The bottom action bar: compact Save + prominent Connect.
    private var actionBar: some View {
        HStack(spacing: 12) {
            Button {
                // Prefill with the ticket's phrase as a recognizable default
                // name; the alert lets the user replace it.
                newProfileName = PairingPhrase.phrase(for: trimmedTicket)
                showSaveNamePrompt = true
            } label: {
                Label("Save", systemImage: "square.and.arrow.down")
            }
            .buttonStyle(AroSecondaryButtonStyle())
            .disabled(!canConnect)
            .opacity(canConnect ? 1 : 0.5)

            Button {
                // Do NOT connect yet — require phrase verification first.
                showVerification = true
            } label: {
                Label("Connect", systemImage: "link")
            }
            .buttonStyle(AroPrimaryButtonStyle(enabled: canConnect))
            .disabled(!canConnect)
        }
    }

    /// Debug-only unattended witness (same pattern as `MainView`'s
    /// `HOLOIROH_AUTOFOCUS_PROMPT`): `simctl`/`devicectl` cannot tap the
    /// ticket editor, so `HOLOIROH_AUTOFOCUS_TICKET=1` focuses it
    /// programmatically -- the exact `focusedField = .ticket` a real tap
    /// performs -- driving the real keyboard and therefore the real
    /// keyboard-toolbar Done bar for a screenshot witness.
    private func autoFocusForWitnessIfNeeded() {
        #if DEBUG
        guard ProcessInfo.processInfo.environment["HOLOIROH_AUTOFOCUS_TICKET"] == "1" else { return }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
            focusedField = .ticket
        }
        #endif
    }
}

#Preview("Pairing - empty") {
    PairingView(onConnect: { _, _ in })
        .environmentObject(ConnectionProfileStore())
}

#Preview("Pairing - filled") {
    PairingView(onConnect: { _, _ in })
        .environmentObject(ConnectionProfileStore())
}
