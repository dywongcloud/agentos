import AVFoundation
import Foundation

/// Plays the optional orb-reaction cue after a send.
///
/// - Reads `soundEnabled`, which defaults to `false`.
/// - Uses the ambient audio session and mixes with other audio.
/// - Honors the device silent-mode setting.
/// - Ignores missing assets, session errors, and player failures.
enum OrbSound {
    private static let player: AVAudioPlayer? = {
        guard let url = Bundle.module.url(
            forResource: "orb_react",
            withExtension: "wav",
            subdirectory: "Sounds"
        ) else { return nil }
        let player = try? AVAudioPlayer(contentsOf: url)
        player?.prepareToPlay()
        return player
    }()

    private static var sessionConfigured = false

    /// Plays the cue when the user enables sound.
    /// Restarts the cue for each reaction.
    /// Does nothing when the asset or player is unavailable.
    static func playReaction() {
        guard UserDefaults.standard.object(forKey: "soundEnabled") as? Bool ?? false else { return }
        configureSessionIfNeeded()
        guard let player else { return }
        player.currentTime = 0
        player.play()
    }

    private static func configureSessionIfNeeded() {
        guard !sessionConfigured else { return }
        sessionConfigured = true
        #if os(iOS)
        let session = AVAudioSession.sharedInstance()
        try? session.setCategory(.ambient, options: [.mixWithOthers])
        try? session.setActive(true)
        #endif
    }
}
