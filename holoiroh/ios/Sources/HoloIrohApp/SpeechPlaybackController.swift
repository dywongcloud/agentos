import Foundation
import AVFoundation

/// Plays Waveform Audio File Format (WAV) data from `ServerMessage.speechReady` through `AVAudioPlayer`.
/// This controller keeps playback separate from both recording paths.
@MainActor
final class SpeechPlaybackController: NSObject, ObservableObject {
    @Published private(set) var isPlaying = false
    @Published var lastError: String?

    private var player: AVAudioPlayer?

    /// Decodes and plays base64-encoded WAV data.
    /// Replaces current playback.
    /// Reports decoding, session, and playback failures through `lastError`.
    func play(base64WavData: String) {
        lastError = nil
        guard let data = Data(base64Encoded: base64WavData) else {
            lastError = "Received speech audio was not valid base64."
            return
        }
        #if os(iOS)
        do {
            try AVAudioSession.sharedInstance().setCategory(.playback, mode: .default)
            try AVAudioSession.sharedInstance().setActive(true)
        } catch {
            lastError = "Audio session setup failed: \(error.localizedDescription)"
            return
        }
        #endif
        do {
            let player = try AVAudioPlayer(data: data)
            player.delegate = self
            guard player.play() else {
                lastError = "Failed to start playback."
                return
            }
            self.player = player
            self.isPlaying = true
        } catch {
            lastError = "Failed to decode speech audio: \(error.localizedDescription)"
        }
    }

    func stop() {
        player?.stop()
        player = nil
        isPlaying = false
    }
}

extension SpeechPlaybackController: AVAudioPlayerDelegate {
    nonisolated func audioPlayerDidFinishPlaying(_ player: AVAudioPlayer, successfully flag: Bool) {
        Task { @MainActor in
            self.isPlaying = false
        }
    }

    nonisolated func audioPlayerDecodeErrorDidOccur(_ player: AVAudioPlayer, error: Error?) {
        Task { @MainActor in
            self.lastError = error?.localizedDescription ?? "Playback decode error"
            self.isPlaying = false
        }
    }
}
