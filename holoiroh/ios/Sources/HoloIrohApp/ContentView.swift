import SwiftUI

/// Placeholder root view. Will become the pairing / live-view / prompt
/// screen described in holoiroh/README.md.
///
/// The prompt text field + microphone button below are the first piece of
/// that eventual screen: tapping the mic starts `VoiceTranscriber` listening
/// via `VoiceTranscriberModel`, live partial results stream into `promptText`
/// as the user speaks, and stopping (tap again, or the recognizer completing
/// naturally) leaves the final transcript in the field ready to send once the
/// control-channel wiring (see PROTOCOL.md's `voice_transcript` message)
/// lands in a later task.
struct ContentView: View {
    @StateObject private var voice = VoiceTranscriberModel()
    @State private var promptText: String = ""

    var body: some View {
        VStack(spacing: 12) {
            Text("HoloIroh")
                .font(.largeTitle)
                .fontWeight(.bold)
            Text("Skeleton -- pairing and live view not yet implemented")
                .font(.footnote)
                .foregroundStyle(.secondary)

            HStack(spacing: 8) {
                TextField("Type a prompt or tap the mic", text: $promptText, axis: .vertical)
                    .textFieldStyle(.roundedBorder)

                Button(action: micTapped) {
                    Image(systemName: voice.isRecording ? "mic.fill" : "mic")
                        .font(.title2)
                        .foregroundStyle(voice.isRecording ? .red : .accentColor)
                        .padding(8)
                }
                .accessibilityLabel(voice.isRecording ? "Stop recording" : "Start voice prompt")
            }
            .padding(.top, 8)

            if voice.isRecording {
                Text("Listening…")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            if let error = voice.lastError {
                Text(error)
                    .font(.caption)
                    .foregroundStyle(.red)
            }
        }
        .padding()
        // Live partial + final transcript updates populate the prompt field
        // as they arrive while a recognition session is running.
        .onChange(of: voice.liveText) { _, newText in
            guard voice.isRecording, !newText.isEmpty else { return }
            promptText = newText
        }
    }

    private func micTapped() {
        Task {
            await voice.toggle()
        }
    }
}

#Preview {
    ContentView()
}
