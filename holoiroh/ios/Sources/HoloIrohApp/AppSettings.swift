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

    /// Configures optional Tinfoil audio transcription through `ClientMessage.transcribeAudio`.
    ///
    /// The default is off. The user must opt in because recorded audio leaves the device.
    /// This setting applies only to the `VoiceTranscriber` microphone tap.
    /// It never applies to system or speaker audio.
    enum TinfoilAudio {
        static let storageKey = "tinfoilAudioTranscriptionEnabled"
        static let enabledByDefault = false
    }
}
