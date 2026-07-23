import SwiftUI

/// The hidden on-device diagnostics screen (opened by shaking the device, or a
/// long-press on the "Aro" title). It surfaces the exact state that recent
/// support reports needed console logs to see: the ConnectionProfileStore's real
/// contents + sqlite health, the last connection error, a rolling event log, and
/// the user-facing feature toggles -- so "saved profiles are empty" type reports
/// self-diagnose on device.
struct DiagnosticsView: View {
    @EnvironmentObject private var profileStore: ConnectionProfileStore
    @EnvironmentObject private var reachability: ReachabilityMonitor
    @ObservedObject private var diagnostics = ConnectionDiagnostics.shared

    @AppStorage("hapticsEnabled") private var hapticsEnabled = true
    @AppStorage("autoConnectEnabled") private var autoConnectEnabled = true
    @AppStorage("soundEnabled") private var soundEnabled = false

    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Form {
                storeSection
                reachabilitySection
                connectionSection
                settingsSection
                Section {
                    Text("Shake the device — or long-press the “Aro” title on the pairing screen — to open this screen.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .navigationTitle("Diagnostics")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
    }

    // MARK: Store state

    private var storeSection: some View {
        Section("Saved profiles (\(profileStore.profiles.count))") {
            LabeledContent("SQLite") {
                Text(profileStore.isOpen ? "open" : "NOT OPEN — in-memory default only")
                    .foregroundStyle(profileStore.isOpen ? .green : .orange)
            }
            LabeledContent("DB file", value: URL(fileURLWithPath: profileStore.databasePath).lastPathComponent)

            if profileStore.profiles.isEmpty {
                Text("⚠️ No profiles — this should be impossible (the default is synthesized). Report this.")
                    .font(.caption)
                    .foregroundStyle(.red)
            }
            ForEach(profileStore.profiles) { profile in
                VStack(alignment: .leading, spacing: 3) {
                    HStack {
                        Text(profile.name).font(.subheadline.weight(.semibold))
                        if profile.id == ConnectionProfileStore.syntheticDefaultID {
                            Text("default")
                                .font(.caption2.weight(.semibold))
                                .padding(.horizontal, 6).padding(.vertical, 1)
                                .background(Color.aroAccent.opacity(0.25), in: Capsule())
                        }
                        Spacer()
                        Text("PIN \(profile.pin.isEmpty ? "—" : profile.pin)")
                            .font(.caption.monospaced())
                    }
                    Text(profile.ticket)
                        .font(.caption2.monospaced())
                        .foregroundStyle(.secondary)
                        .lineLimit(3)
                        .textSelection(.enabled)
                    Text("phrase: \(profile.phrase)")
                        .font(.caption2)
                        .foregroundStyle(.tertiary)
                }
                .padding(.vertical, 2)
            }
        }
    }

    // MARK: Daemon reachability

    private var reachabilitySection: some View {
        Section("Daemon reachability") {
            LabeledContent("Dev Mac") {
                ReachabilityPill(state: reachability.state)
            }
            if let at = reachability.lastCheckedAt {
                LabeledContent("Last checked", value: at.formatted(date: .omitted, time: .standard))
            }
            Button {
                reachability.checkNow()
            } label: {
                Label("Check now", systemImage: "dot.radiowaves.left.and.right")
            }
        }
    }

    // MARK: Connection

    private var connectionSection: some View {
        Section("Last connection") {
            if let error = diagnostics.lastError {
                LabeledContent("Error") { Text(error).foregroundStyle(.orange) }
                if let ticket = diagnostics.lastErrorTicketPrefix {
                    LabeledContent("Ticket", value: ticket)
                }
                if let at = diagnostics.lastErrorAt {
                    LabeledContent("At", value: at.formatted(date: .omitted, time: .standard))
                }
            } else {
                Text("No connection errors recorded this session.")
                    .foregroundStyle(.secondary)
            }
            if !diagnostics.log.isEmpty {
                DisclosureGroup("Event log (\(diagnostics.log.count))") {
                    ForEach(Array(diagnostics.log.enumerated().reversed()), id: \.offset) { _, line in
                        Text(line)
                            .font(.caption2.monospaced())
                            .textSelection(.enabled)
                    }
                }
            }
        }
    }

    // MARK: Settings

    private var settingsSection: some View {
        Section("Settings") {
            Toggle("Haptics", isOn: $hapticsEnabled)
            Toggle("Auto-connect on launch", isOn: $autoConnectEnabled)
            Toggle("Orb sound", isOn: $soundEnabled)
        }
    }
}

#Preview {
    DiagnosticsView()
        .environmentObject(ConnectionProfileStore())
        .environmentObject(ReachabilityMonitor(ticket: ""))
}
