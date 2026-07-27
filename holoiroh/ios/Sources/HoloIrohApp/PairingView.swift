import SwiftUI

struct PairingView: View {
    let onConnect: (_ ticket: String, _ pin: String) -> Void

    var onInteract: () -> Void = {}

    @State private var ticketText: String = ""
    @State private var pinText: String = ""
    @State private var showScanner = false
    @State private var showVerification = false
    @State private var scanError: String?

    private enum Field: Hashable {
        case ticket
        case pin
    }
    @FocusState private var focusedField: Field?

    @EnvironmentObject private var profileStore: ConnectionProfileStore

    @EnvironmentObject private var reachability: ReachabilityMonitor
    @State private var showSaveNamePrompt = false
    @State private var newProfileName = ""

    @AppStorage(AppSettings.AutoConnect.storageKey)
    private var autoConnectEnabled = AppSettings.AutoConnect.enabledByDefault

    private var trimmedTicket: String {
        ticketText.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private var canConnect: Bool {
        !trimmedTicket.isEmpty
    }

    var body: some View {
        ScrollView {
            VStack(spacing: 18) {
                header
                    .padding(.top, 36)

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
        .background(
            PairingBackdrop()
                .contentShape(Rectangle())
                .onTapGesture { focusedField = nil }
        )
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
        .onAppear {
            runUnattendedWitnessHooksIfNeeded()
            reachability.checkNow()
        }
        .onChange(of: focusedField) { _, newValue in
            if newValue != nil { onInteract() }
        }
    }

    // MARK: - Sections

    private var header: some View {
        VStack(spacing: 14) {
            AroOrbMark(diameter: 58)
            VStack(spacing: 6) {
                AroWordmark(size: 46)
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

    private var scanButton: some View {
        Button {
            onInteract()
            scanError = nil
            showScanner = true
        } label: {
            Label("Scan QR code", systemImage: "qrcode.viewfinder")
                .frame(maxWidth: .infinity)
        }
        .buttonStyle(AroSecondaryButtonStyle())
    }

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

    private var savedProfilesSection: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                AroFieldLabel(title: "Saved profiles", systemImage: "bookmark")
                Spacer()
                Button {
                    onInteract()
                    reachability.checkNow()
                } label: {
                    Image(systemName: "arrow.clockwise")
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(.white.opacity(0.6))
                        .rotationEffect(.degrees(reachability.state == .checking ? 360 : 0))
                        .animation(reachability.state == .checking ? .linear(duration: 0.8).repeatForever(autoreverses: false) : .default, value: reachability.state)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Refresh daemon reachability")
            }
            VStack(spacing: 10) {
                    ForEach(profileStore.profiles) { profile in
                        Button {
                            onInteract()
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
                                    HStack(spacing: 8) {
                                        Text(profile.name)
                                            .font(.subheadline.weight(.semibold))
                                            .foregroundStyle(.white)
                                        if profile.ticket == profileStore.autoConnectProfile?.ticket {
                                            ReachabilityPill(state: reachability.state)
                                        }
                                    }
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
            autoConnectToggle
        }
    }

    private var autoConnectToggle: some View {
        Toggle(isOn: $autoConnectEnabled) {
            VStack(alignment: .leading, spacing: 2) {
                Text("Connect automatically on launch")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.white.opacity(0.9))
                Text("Skips this screen and opens your last profile")
                    .font(.caption2)
                    .foregroundStyle(.white.opacity(0.45))
            }
        }
        .tint(Color.aroAccent)
        .onChange(of: autoConnectEnabled) { _, _ in onInteract() }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
        .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(Color.white.opacity(0.10), lineWidth: 1)
        )
        .accessibilityIdentifier("autoConnectToggle")
    }

    private var actionBar: some View {
        HStack(spacing: 12) {
            Button {
                onInteract()
                newProfileName = PairingPhrase.phrase(for: trimmedTicket)
                showSaveNamePrompt = true
            } label: {
                Label("Save", systemImage: "square.and.arrow.down")
            }
            .buttonStyle(AroSecondaryButtonStyle())
            .disabled(!canConnect)
            .opacity(canConnect ? 1 : 0.5)

            Button {
                onInteract()
                showVerification = true
            } label: {
                Label("Connect", systemImage: "link")
            }
            .buttonStyle(AroPrimaryButtonStyle(enabled: canConnect))
            .disabled(!canConnect)
        }
    }

    private func runUnattendedWitnessHooksIfNeeded() {
        #if DEBUG
        let env = ProcessInfo.processInfo.environment
        if env["HOLOIROH_AUTOFOCUS_TICKET"] == "1" {
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
                focusedField = .ticket
            }
        }
        if env["HOLOIROH_WITNESS_OPEN_SCANNER"] == "1" {
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) {
                scanError = nil
                showScanner = true
            }
        }
        if env["HOLOIROH_WITNESS_TAP_SAVED_PROFILE"] == "1" {
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) {
                guard let profile = profileStore.profiles.first else { return }
                NSLog("PairingView: witness tapping saved profile \(profile.name)")
                onInteract()
                onConnect(profile.ticket, profile.pin)
            }
        }
        #endif
    }
}

#Preview("Pairing - empty") {
    PairingView(onConnect: { _, _ in })
        .environmentObject(ConnectionProfileStore())
        .environmentObject(ReachabilityMonitor(ticket: ""))
}

#Preview("Pairing - filled") {
    PairingView(onConnect: { _, _ in })
        .environmentObject(ConnectionProfileStore())
        .environmentObject(ReachabilityMonitor(ticket: ""))
}
