import Foundation
import AVFoundation

/// Records the device's own microphone to a WAV file for `ClientMessage.transcribeAudio`
/// (opt-in Tinfoil-backed transcription). Deliberately a SEPARATE class from
/// `VoiceTranscriber`/`VoiceTranscriberModel` (the default on-device path) rather than a shared
/// abstraction: this class's entire reason to exist is the consent-scope invariant --
/// **microphone input only, never system/speaker audio** -- and keeping it as its own small,
/// auditable recorder makes that invariant obvious at the type level (there is no code path
/// here that could accidentally capture anything else) rather than one flag buried inside a
/// larger shared recorder. See `tinfoil-audio-consent-scope` (PRD) and `tinfoil_audio.rs`'s
/// module doc for the full rationale.
@MainActor
final class TinfoilAudioRecorder: NSObject, ObservableObject {
    @Published private(set) var isRecording = false
    @Published var lastError: String?

    private var recorder: AVAudioRecorder?
    private var recordingURL: URL?

    /// Requests microphone permission (if needed) and starts recording to a temp WAV file.
    func start() async {
        guard !isRecording else { return }
        lastError = nil

        let granted: Bool
        #if os(iOS)
        if #available(iOS 17.0, *) {
            granted = await AVAudioApplication.requestRecordPermission()
        } else {
            granted = await withCheckedContinuation { (continuation: CheckedContinuation<Bool, Never>) in
                AVAudioSession.sharedInstance().requestRecordPermission { continuation.resume(returning: $0) }
            }
        }
        #else
        granted = true
        #endif
        guard granted else {
            lastError = "Microphone access was not authorized."
            return
        }

        #if os(iOS)
        do {
            let session = AVAudioSession.sharedInstance()
            try session.setCategory(.record, mode: .default)
            try session.setActive(true)
        } catch {
            lastError = "Audio session setup failed: \(error.localizedDescription)"
            return
        }
        #endif

        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString)
            .appendingPathExtension("wav")
        let settings: [String: Any] = [
            AVFormatIDKey: kAudioFormatLinearPCM,
            AVSampleRateKey: 16_000,
            AVNumberOfChannelsKey: 1,
            AVLinearPCMBitDepthKey: 16,
            AVLinearPCMIsFloatKey: false,
        ]

        do {
            let recorder = try AVAudioRecorder(url: url, settings: settings)
            recorder.delegate = self
            guard recorder.record() else {
                lastError = "Failed to start recording."
                return
            }
            self.recorder = recorder
            self.recordingURL = url
            self.isRecording = true
        } catch {
            lastError = "Failed to start recording: \(error.localizedDescription)"
        }
    }

    /// Stops recording and returns the recorded WAV file's bytes, or `nil` on failure.
    func stopAndReadBytes() -> Data? {
        guard isRecording, let recorder, let url = recordingURL else { return nil }
        recorder.stop()
        isRecording = false
        self.recorder = nil
        self.recordingURL = nil
        #if os(iOS)
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
        #endif
        defer { try? FileManager.default.removeItem(at: url) }
        return try? Data(contentsOf: url)
    }
}

extension TinfoilAudioRecorder: AVAudioRecorderDelegate {
    nonisolated func audioRecorderEncodeErrorDidOccur(_ recorder: AVAudioRecorder, error: Error?) {
        Task { @MainActor in
            self.lastError = error?.localizedDescription ?? "Recording encode error"
            self.isRecording = false
        }
    }
}
