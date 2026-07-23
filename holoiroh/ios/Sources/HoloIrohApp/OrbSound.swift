import AVFoundation
import Foundation

/// Plays the short, script-generated orb-reaction cue when the orb reacts to a
/// send. Opt-in behind `@AppStorage("soundEnabled")` (default OFF), and routed
/// through the AMBIENT audio session so it honors the ringer/silent switch and
/// never interrupts other audio. Entirely best-effort: a missing asset, session
/// error, or player failure is silently ignored -- the cue is pure polish and
/// must never affect (or block) the visual reaction.
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

    /// Plays the cue if the user enabled sound. Resets to the start so rapid
    /// back-to-back reactions each retrigger it. No-op when disabled or the
    /// asset/player is unavailable.
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
