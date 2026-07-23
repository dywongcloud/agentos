import SwiftUI
import UIKit

extension UIDevice {
    /// Posted whenever the device is physically shaken.
    static let deviceDidShakeNotification = Notification.Name("HoloIrohDeviceDidShake")
}

extension UIWindow {
    /// Turn the hardware shake gesture into an app notification. This is the
    /// standard SwiftUI-era hook for a shake: the motion event bubbles up the
    /// responder chain to the key window regardless of what has first-responder.
    open override func motionEnded(_ motion: UIEvent.EventSubtype, with event: UIEvent?) {
        super.motionEnded(motion, with: event)
        if motion == .motionShake {
            NotificationCenter.default.post(name: UIDevice.deviceDidShakeNotification, object: nil)
        }
    }
}

private struct ShakeDetector: ViewModifier {
    let action: () -> Void
    func body(content: Content) -> some View {
        content.onReceive(NotificationCenter.default.publisher(for: UIDevice.deviceDidShakeNotification)) { _ in
            action()
        }
    }
}

extension View {
    /// Run `action` when the device is shaken -- used to open the hidden
    /// diagnostics screen. A discreet long-press affordance is provided too, so
    /// diagnostics are reachable without shaking.
    func onShake(perform action: @escaping () -> Void) -> some View {
        modifier(ShakeDetector(action: action))
    }
}
