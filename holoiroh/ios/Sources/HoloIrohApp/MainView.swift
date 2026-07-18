import SwiftUI

/// Main screen shown once the app has "connected" (see `ContentView`'s
/// navigation wiring -- there is no real control-channel/iroh connection
/// yet, only the pairing hand-off).
///
/// Layout mirrors the four elements called out in the iOS-side README
/// section ("Pairing" / "Live view" / "Prompts" / "Status"):
/// 1. A video preview area -- now a real `VideoRenderView`
///    (`AVSampleBufferDisplayLayer`-backed) bound to a `VideoFrameSource`.
///    The *render* path is real; the *source* bound here is a synthetic
///    on-device generator (`SyntheticVideoFrameSource`) standing in for
///    the not-yet-wired `iroh-live` network source, so the preview
///    animates today and the real source drops into the same binding
///    later without touching this view.
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
    @StateObject private var voice = VoiceTranscriberModel()
    @State private var logEntries: [LogEntry] = [
        LogEntry(message: .status(text: "paired -- control channel not yet connected"))
    ]

    /// The frame producer bound to the video surface. Held with `@State`
    /// so it keeps a stable identity across view updates (a fresh source
    /// on every re-render would restart the display link each time). This
    /// is the single swap point: replacing `SyntheticVideoFrameSource()`
    /// with the real `iroh-live` source (once `ios-bridge` is wired) is the
    /// only change the live-view feature needs here.
    @State private var frameSource: VideoFrameSource = SyntheticVideoFrameSource()

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
        // Live partial + final transcript updates populate the prompt field
        // as they arrive while a recognition session is running.
        .onChange(of: voice.liveText) { _, newText in
            guard voice.isRecording, !newText.isEmpty else { return }
            promptText = newText
        }
        .onChange(of: voice.lastError) { _, error in
            guard let error else { return }
            logEntries.append(LogEntry(message: .error(text: error)))
        }
    }

    // MARK: - Video preview

    private var videoPreview: some View {
        // Real render surface: a `VideoRenderView` (AVSampleBufferDisplayLayer)
        // bound to `frameSource`. The synthetic source makes it animate
        // today; the layout (fixed 16:9, rounded clip) is unchanged from
        // the old placeholder so nothing jumps when the real iroh-live
        // source replaces the synthetic one at `frameSource`'s init.
        VideoRenderView(source: frameSource)
            .background(Color.black)
            .aspectRatio(16.0 / 9.0, contentMode: .fit)
            .clipShape(RoundedRectangle(cornerRadius: 12))
            .accessibilityLabel("Live video preview (synthetic test frames until the network source is connected)")
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
                Image(systemName: voice.isRecording ? "mic.fill" : "mic")
                    .foregroundStyle(voice.isRecording ? Color.red : Color.accentColor)
            }
            .buttonStyle(.bordered)
            .accessibilityLabel(voice.isRecording ? "Stop recording" : "Start voice prompt")

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
        // Real on-device transcription via `VoiceTranscriberModel` (Speech
        // framework). `ClientMessage.voiceTranscript` control-channel send
        // is still a later task -- today the final transcript just lands in
        // `promptText`, same as if the user had typed it.
        let wasRecording = voice.isRecording
        Task {
            await voice.toggle()
        }
        logEntries.append(
            LogEntry(message: .status(text: wasRecording ? "stopped listening" : "listening…"))
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
