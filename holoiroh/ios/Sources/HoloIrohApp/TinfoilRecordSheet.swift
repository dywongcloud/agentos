import SwiftUI

/// Record-and-transcribe sheet for the opt-in Tinfoil audio path
/// (`TinfoilAudioRecorder` + `ClientMessage.transcribeAudio`). Deliberately separate from the
/// existing mic button (which drives the default on-device `VoiceTranscriberModel` path) so the
/// two recording flows never share state -- see `TinfoilAudioRecorder`'s doc comment.
struct TinfoilRecordSheet: View {
    @ObservedObject var recorder: TinfoilAudioRecorder
    let onSend: (Data) -> Void

    @Environment(\.dismiss) private var dismiss
    @Binding var resultText: String?
    @Binding var resultError: String?

    var body: some View {
        NavigationStack {
            VStack(spacing: 24) {
                Image(systemName: recorder.isRecording ? "waveform" : "mic.circle")
                    .font(.system(size: 64))
                    .foregroundStyle(recorder.isRecording ? .red : .secondary)
                    .symbolEffect(.pulse, isActive: recorder.isRecording)

                Text(recorder.isRecording ? "Recording your microphone…" : "Tap to record")
                    .font(.headline)

                Text("Your own microphone audio is sent to Tinfoil's confidential-computing cloud for transcription. This never captures system or call audio.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .padding(.horizontal)

                Button {
                    Task {
                        if recorder.isRecording {
                            if let data = recorder.stopAndReadBytes() {
                                onSend(data)
                            }
                        } else {
                            resultText = nil
                            resultError = nil
                            await recorder.start()
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
                        if recorder.isRecording { _ = recorder.stopAndReadBytes() }
                        dismiss()
                    }
                }
            }
        }
    }
}
