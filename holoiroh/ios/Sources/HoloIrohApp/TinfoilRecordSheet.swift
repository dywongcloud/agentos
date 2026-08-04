import HoloIrohMicrophoneCapture
import SwiftUI

/// Records microphone audio for confidential transcription through Tinfoil.
/// This flow uses `TinfoilAudioRecorder` and `ClientMessage.transcribeAudio`.
/// It does not share state with `VoiceTranscriberModel`.
struct TinfoilRecordSheet: View {
    @ObservedObject var recorder: TinfoilAudioRecorder
    let onSend: (CapturedMicrophoneAudio) -> Void

    @Environment(\.dismiss) private var dismiss
    @Binding var resultText: String?
    @Binding var resultError: String?
    @State private var startTask: Task<Void, Never>?

    var body: some View {
        NavigationStack {
            VStack(spacing: 24) {
                Image(systemName: recorder.isRecording ? "waveform" : "mic.circle")
                    .font(.system(size: 64))
                    .foregroundStyle(recorder.isRecording ? .red : .secondary)
                    .symbolEffect(.pulse, isActive: recorder.isRecording)

                Text(recorder.isRecording ? "Recording your microphone…" : "Tap to record")
                    .font(.headline)

                Text("Only audio picked up by your device's microphone is sent to Tinfoil's confidential-computing cloud for transcription. This does not capture a digital system-audio or call-audio mix, but the microphone may capture nearby people, speakers, or other ambient audio. Inspect this connection's cryptographic proof in Diagnostics → Verification Center.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .padding(.horizontal)

                Button {
                    if recorder.isRecording {
                        startTask?.cancel()
                        startTask = nil
                        if let capture = recorder.stopAndCapture() {
                            onSend(capture)
                        }
                    } else {
                        resultText = nil
                        resultError = nil
                        startTask?.cancel()
                        startTask = Task {
                            await recorder.start()
                            if Task.isCancelled { recorder.discard() }
                        }
                    }
                } label: {
                    Text(recorder.isRecording ? "Stop & Transcribe" : "Start Recording")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .tint(recorder.isRecording ? .red : .accentColor)
                .padding(.horizontal)

                if let error = recorder.lastError {
                    Text(error).foregroundStyle(.red).font(.caption)
                }
                if let resultText {
                    Text(resultText).textSelection(.enabled).padding(.horizontal)
                }
                if let resultError {
                    Text(resultError).foregroundStyle(.red).padding(.horizontal)
                }

                Spacer()
            }
            .padding(.top, 32)
            .navigationTitle("Tinfoil Transcription")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") {
                        startTask?.cancel()
                        startTask = nil
                        recorder.discard()
                        dismiss()
                    }
                }
            }
            .onDisappear {
                startTask?.cancel()
                startTask = nil
                recorder.discard()
            }
        }
    }
}
