import Foundation
#if canImport(UIKit)
import UIKit
#endif

/// Provides consistent haptic feedback for the app.
/// Reads `hapticsEnabled`, which defaults to `true`.
/// Calls do nothing on platforms without UIKit haptic generators.
@MainActor
enum Haptics {
    /// Defines the app haptic events.
    enum Event {
        case connect        // a control channel just connected
        case introReveal    // the intro finished and handed off to pairing
        case takeControl    // entered/left hands-on remote control
        case success        // a positive confirmation
        case warning        // something needs attention
    }

    /// Indicates whether the user enables haptics. A missing setting returns `true`.
    static var isEnabled: Bool {
        UserDefaults.standard.object(forKey: "hapticsEnabled") as? Bool ?? true
    }

    static func fire(_ event: Event) {
        guard isEnabled else { return }
        #if canImport(UIKit)
        switch event {
        case .connect:
            UINotificationFeedbackGenerator().notificationOccurred(.success)
        case .success:
            UINotificationFeedbackGenerator().notificationOccurred(.success)
        case .warning:
            UINotificationFeedbackGenerator().notificationOccurred(.warning)
        case .introReveal:
            UIImpactFeedbackGenerator(style: .soft).impactOccurred()
        case .takeControl:
            UIImpactFeedbackGenerator(style: .medium).impactOccurred()
        }
        #else
        _ = event
        #endif
    }
}
