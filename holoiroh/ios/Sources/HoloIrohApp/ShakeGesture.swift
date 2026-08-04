import SwiftUI

#if canImport(UIKit)
import UIKit

extension UIDevice {
    /// Identifies a physical device-shake notification.
    static let deviceDidShakeNotification = Notification.Name("HoloIrohDeviceDidShake")
}

extension UIWindow {
    /// Posts an app notification after the window receives a shake event.
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
    /// Runs `action` after a device shake.
    /// The app also provides a long-press path to diagnostics.
    func onShake(perform action: @escaping () -> Void) -> some View {
        modifier(ShakeDetector(action: action))
    }
}
#else
extension View {
    func onShake(perform action: @escaping () -> Void) -> some View {
        self
    }
}
#endif
