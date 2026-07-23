import UIKit

/// One place for tactile feedback, so the app's haptics are consistent and
/// centrally muteable. Honors the `hapticsEnabled` setting (default ON) and is
/// safe to call from anywhere on the main actor -- e.g. a connection-completion
/// closure, an `.onChange`, or a button action. No-ops cleanly on devices
/// without a Taptic Engine (the generators simply do nothing).
@MainActor
enum Haptics {
    /// The tactile vocabulary of the app.
    enum Event {
        case connect        // a control channel just connected
        case introReveal    // the intro finished and handed off to pairing
        case takeControl    // entered/left hands-on remote control
        case success        // a positive confirmation
        case warning        // something needs attention
    }

    /// Whether haptics are enabled. UserDefaults has no key by default, which we
    /// treat as ON (opt-out), matching the iOS convention for system feedback.
    static var isEnabled: Bool {
        UserDefaults.standard.object(forKey: "hapticsEnabled") as? Bool ?? true
    }

    static func fire(_ event: Event) {
        guard isEnabled else { return }
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
    }
}
