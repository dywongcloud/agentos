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
}
