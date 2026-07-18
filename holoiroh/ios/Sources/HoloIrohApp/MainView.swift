import SwiftUI

/// Main screen shown once the app has "connected" (see `ContentView`'s
/// navigation wiring -- there is no real control-channel/iroh connection
/// yet, only the pairing hand-off).
///
/// Layout mirrors the four elements called out in the iOS-side README
/// section ("Pairing" / "Live view" / "Prompts" / "Status"):
/// 1. A video preview area -- placeholder solid-black rectangle for now,
///    to be replaced by real `iroh-live` frame rendering in a later task.
/// 2. A text field + Send button for prompts.
/// 3. A microphone button (placeholder action -- no on-device
///    transcription wired up yet).
/// 4. A scrolling status/log list of `ServerMessage`-equivalent strings.
struct MainView: View {
    /// The ticket this session paired with, shown in the header so the
    /// user can confirm which daemon they connected to.
    let ticket: String

    /// Returns to `PairingView`.
    let onDisconnect: () -> Void

    @State private var promptText: String = ""
    @State private var isRecording: Bool = false
    @State private var logEntries: [LogEntry] = [
        LogEntry(message: .status(text: "paired -- control channel not yet connected"))
    ]

    var body: some View {
        VStack(spacing: 0) {
            videoPreview
                .padding(.horizontal)
                .padding(.top, 12)

            logPanel
                .padding(.top, 12)

            promptBar
                .padding()
        }
        .navigationTitle("HoloIroh")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button("Disconnect", role: .destructive) {
                    onDisconnect()
                }
            }
        }
    }

    // MARK: - Video preview

    private var videoPreview: some View {
        // Placeholder for the real iroh-live video surface (later task).
        // Fixed 16:9 aspect ratio approximates a desktop-capture frame so
        // the rest of the layout doesn't jump once real video lands.
        ZStack {
            Color.black
            VStack(spacing: 6) {
                Image(systemName: "video.slash")
                    .font(.title2)
                    .foregroundStyle(.white.opacity(0.6))
                Text("Video preview placeholder")
                    .font(.caption)
                    .foregroundStyle(.white.opacity(0.6))
            }
        }
        .aspectRatio(16.0 / 9.0, contentMode: .fit)
        .clipShape(RoundedRectangle(cornerRadius: 12))
        .accessibilityLabel("Video preview placeholder, not yet connected to a live stream")
    }

    // MARK: - Status / log panel

    private var logPanel: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("Status")
                .font(.caption)
                .foregroundStyle(.secondary)
                .padding(.horizontal)

            if logEntries.isEmpty {
                Text("No activity yet")
                    .font(.footnote)
                    .foregroundStyle(.tertiary)
                    .frame(maxWidth: .infinity, alignment: .center)
                    .padding()
            } else {
                ScrollViewReader { proxy in
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 6) {
                            ForEach(logEntries) { entry in
                                logRow(entry)
                                    .id(entry.id)
                            }
                        }
                        .padding(.horizontal)
                    }
                    .onChange(of: logEntries.count) {
                        if let lastID = logEntries.last?.id {
                            withAnimation {
                                proxy.scrollTo(lastID, anchor: .bottom)
                            }
                        }
                    }
                }
            }
        }
        .frame(maxHeight: .infinity)
        .background(Color(.secondarySystemBackground))
    }

    private func logRow(_ entry: LogEntry) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Text(entry.formattedTime)
                .font(.system(.caption2, design: .monospaced))
                .foregroundStyle(.tertiary)

            Text(entry.message.kindLabel)
                .font(.system(.caption2, design: .monospaced))
                .fontWeight(.semibold)
                .foregroundStyle(logColor(for: entry.message))
                .frame(width: 64, alignment: .leading)

            Text(entry.message.displayText)
                .font(.caption)
                .foregroundStyle(.primary)
                .lineLimit(3)
                .fixedSize(horizontal: false, vertical: true)

            Spacer(minLength: 0)
        }
        .padding(.vertical, 2)
    }

    private func logColor(for message: ServerMessage) -> Color {
        switch message {
        case .ack: return .secondary
        case .status: return .blue
        case .taskProgress: return .orange
        case .error: return .red
        }
    }

    // MARK: - Prompt bar

    private var promptBar: some View {
        HStack(spacing: 8) {
            TextField("Type a prompt…", text: $promptText, axis: .vertical)
                .textFieldStyle(.roundedBorder)
                .lineLimit(1...4)
                .onSubmit(sendPrompt)

            Button {
                toggleMicrophone()
            } label: {
                Image(systemName: isRecording ? "mic.fill" : "mic")
                    .foregroundStyle(isRecording ? Color.red : Color.accentColor)
            }
            .buttonStyle(.bordered)
            .accessibilityLabel(isRecording ? "Stop recording" : "Start voice prompt")

            Button {
                sendPrompt()
            } label: {
                Image(systemName: "arrow.up.circle.fill")
                    .font(.title2)
            }
            .buttonStyle(.plain)
            .disabled(promptText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            .accessibilityLabel("Send prompt")
        }
    }

    private func sendPrompt() {
        let trimmed = promptText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }

        // No real control-channel transport yet -- this simulates the
        // round trip locally (an immediate ack) so the log list is
        // demonstrably live rather than static mock data. A future task
        // replaces this with an actual `ClientMessage.prompt` send and
        // real `ServerMessage` responses streamed back from the daemon.
        logEntries.append(LogEntry(message: .status(text: "sent: \"\(trimmed)\"")))
        logEntries.append(LogEntry(message: .ack(text: nil)))

        promptText = ""
    }

    private func toggleMicrophone() {
        // Placeholder action -- on-device transcription + ClientMessage
        // .voiceTranscript wiring is a later task. Toggling state here
        // only drives the button's own visual affordance.
        isRecording.toggle()
        logEntries.append(
            LogEntry(message: .status(text: isRecording ? "listening… (placeholder)" : "stopped listening (placeholder)"))
        )
    }
}

#Preview("Main - with log") {
    NavigationStack {
        MainView(ticket: "iroh-live:example-ticket", onDisconnect: {})
    }
}

#Preview("Main - empty log") {
    NavigationStack {
        MainView(ticket: "iroh-live:example-ticket", onDisconnect: {})
    }
}
