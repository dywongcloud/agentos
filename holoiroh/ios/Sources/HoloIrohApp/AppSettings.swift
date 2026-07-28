import Foundation

enum AppSettings {
    enum AutoConnect {
        static let storageKey = "autoConnectEnabled"

        static let enabledByDefault = false

        private static let optInDefaultAppliedKey = "autoConnectOptInDefaultApplied"

        static func applyOptInDefaultOnce(in defaults: UserDefaults = .standard) {
            guard !defaults.bool(forKey: optInDefaultAppliedKey) else { return }
            defaults.set(enabledByDefault, forKey: storageKey)
            defaults.set(true, forKey: optInDefaultAppliedKey)
        }
    }

    /// Opt-in toggle for Tinfoil-backed audio transcription (`ClientMessage.transcribeAudio`)
    /// as an alternative to the default on-device `VoiceTranscriber` path. Default OFF: sending
    /// this message means the recorded audio leaves the device to Tinfoil's confidential
    /// computing cloud, so the user must explicitly opt in -- see `tinfoil-audio-consent-scope`
    /// (PRD) and `tinfoil_audio.rs`'s module doc for the full rationale. Only ever wired to
    /// `VoiceTranscriber`'s own mic tap (never system/speaker audio) wherever this is read.
    enum TinfoilAudio {
        static let storageKey = "tinfoilAudioTranscriptionEnabled"
        static let enabledByDefault = false
    }
}
