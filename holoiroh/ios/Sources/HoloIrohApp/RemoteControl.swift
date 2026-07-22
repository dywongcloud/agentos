import CoreGraphics
import Foundation

/// Swift mirror of `holoiroh-wire`'s `RemoteControlEvent` -- the nested action
/// of `ClientMessage.remoteControl`, sent when the user escalates and touches
/// the live-share view to drive the Mac directly. Serializes as
/// `{"action": ..., ...}` with NORMALIZED `0..1` coordinates; the daemon maps
/// them to real display points (see `PROTOCOL.md` / `mac-daemon/src/remote_input.rs`).
enum RemoteControlEvent: Codable, Equatable {
    /// Escalate to hands-on control (the daemon pauses any active agent turn).
    case takeControl
    /// Release control (the daemon resumes the paused turn).
    case releaseControl
    case move(x: Double, y: Double)
    case button(x: Double, y: Double, button: MouseButton, down: Bool)
    case click(x: Double, y: Double, button: MouseButton, count: Int)
    case scroll(x: Double, y: Double, dx: Double, dy: Double)
    case text(String)
    case key(key: String, down: Bool)

    enum MouseButton: String, Codable, Equatable { case left, right }

    private enum CodingKeys: String, CodingKey {
        case action, x, y, button, down, count, dx, dy, text, key
    }
    private enum Action: String, Codable {
        case takeControl = "take_control"
        case releaseControl = "release_control"
        case move, button, click, scroll, text, key
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        switch try c.decode(Action.self, forKey: .action) {
        case .takeControl: self = .takeControl
        case .releaseControl: self = .releaseControl
        case .move:
            self = .move(x: try c.decode(Double.self, forKey: .x), y: try c.decode(Double.self, forKey: .y))
        case .button:
            self = .button(
                x: try c.decode(Double.self, forKey: .x), y: try c.decode(Double.self, forKey: .y),
                button: try c.decode(MouseButton.self, forKey: .button), down: try c.decode(Bool.self, forKey: .down))
        case .click:
            self = .click(
                x: try c.decode(Double.self, forKey: .x), y: try c.decode(Double.self, forKey: .y),
                button: try c.decode(MouseButton.self, forKey: .button), count: try c.decode(Int.self, forKey: .count))
        case .scroll:
            self = .scroll(
                x: try c.decode(Double.self, forKey: .x), y: try c.decode(Double.self, forKey: .y),
                dx: try c.decode(Double.self, forKey: .dx), dy: try c.decode(Double.self, forKey: .dy))
        case .text:
            self = .text(try c.decode(String.self, forKey: .text))
        case .key:
            self = .key(key: try c.decode(String.self, forKey: .key), down: try c.decode(Bool.self, forKey: .down))
        }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .takeControl: try c.encode(Action.takeControl, forKey: .action)
        case .releaseControl: try c.encode(Action.releaseControl, forKey: .action)
        case .move(let x, let y):
            try c.encode(Action.move, forKey: .action)
            try c.encode(x, forKey: .x); try c.encode(y, forKey: .y)
        case .button(let x, let y, let b, let d):
            try c.encode(Action.button, forKey: .action)
            try c.encode(x, forKey: .x); try c.encode(y, forKey: .y)
            try c.encode(b, forKey: .button); try c.encode(d, forKey: .down)
        case .click(let x, let y, let b, let n):
            try c.encode(Action.click, forKey: .action)
            try c.encode(x, forKey: .x); try c.encode(y, forKey: .y)
            try c.encode(b, forKey: .button); try c.encode(n, forKey: .count)
        case .scroll(let x, let y, let dx, let dy):
            try c.encode(Action.scroll, forKey: .action)
            try c.encode(x, forKey: .x); try c.encode(y, forKey: .y)
            try c.encode(dx, forKey: .dx); try c.encode(dy, forKey: .dy)
        case .text(let t):
            try c.encode(Action.text, forKey: .action); try c.encode(t, forKey: .text)
        case .key(let k, let d):
            try c.encode(Action.key, forKey: .action)
            try c.encode(k, forKey: .key); try c.encode(d, forKey: .down)
        }
    }
}

/// Map a touch point in the live-view's coordinate space to a NORMALIZED
/// (`0..1`) point in the video frame, accounting for `AVLayerVideoGravity`
/// `.resizeAspect` letterboxing: the video is aspect-fit, so there are bars on
/// the axis where the view aspect differs from the frame aspect, and a touch in
/// a bar is outside the video. Returns `nil` for a bar touch (or bad sizes) so
/// the caller can ignore it rather than send a wildly-off coordinate.
///
/// Pure and self-contained so it is exercised directly by the app's own
/// build-time sanity checks -- no view or device needed.
func normalizedInVideo(touch: CGPoint, viewSize: CGSize, frameSize: CGSize) -> CGPoint? {
    guard viewSize.width > 0, viewSize.height > 0, frameSize.width > 0, frameSize.height > 0 else {
        return nil
    }
    let viewAspect = viewSize.width / viewSize.height
    let frameAspect = frameSize.width / frameSize.height
    // The displayed video rect within the view (aspect-fit).
    var vw = viewSize.width
    var vh = viewSize.height
    var ox: CGFloat = 0
    var oy: CGFloat = 0
    if frameAspect > viewAspect {
        // Video is relatively wider: full view width, letterbox top+bottom.
        vh = viewSize.width / frameAspect
        oy = (viewSize.height - vh) / 2
    } else {
        // Video is relatively taller: full view height, letterbox left+right.
        vw = viewSize.height * frameAspect
        ox = (viewSize.width - vw) / 2
    }
    let nx = (touch.x - ox) / vw
    let ny = (touch.y - oy) / vh
    if nx < 0 || nx > 1 || ny < 0 || ny > 1 {
        return nil
    }
    return CGPoint(x: nx, y: ny)
}

#if canImport(UIKit)
import SwiftUI
import UIKit

/// A transparent touch surface laid over the live-share video while the user is
/// in hands-on control. Translates gestures into `RemoteControlEvent`s with
/// letterbox-correct normalized coordinates (via `normalizedInVideo`):
/// - a tap -> a click at that point,
/// - a one-finger drag -> button-down, moves, button-up (pointer/drag),
/// - a two-finger drag -> scroll by the pan delta.
///
/// UIKit-backed (a `UIViewRepresentable`) because SwiftUI gestures can't cleanly
/// distinguish finger count, which is exactly what separates "move the pointer"
/// from "scroll".
struct RemoteControlSurface: UIViewRepresentable {
    /// The most recent video frame's pixel size, for the aspect-fit mapping.
    /// `nil` falls back to filling the view (no letterbox correction).
    var frameSize: CGSize?
    /// Sends one remote-control action over the control channel.
    var onEvent: (RemoteControlEvent) -> Void

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeUIView(context: Context) -> UIView {
        let v = UIView()
        v.backgroundColor = .clear
        v.isMultipleTouchEnabled = true

        let tap = UITapGestureRecognizer(target: context.coordinator, action: #selector(Coordinator.onTap(_:)))
        v.addGestureRecognizer(tap)

        let pan1 = UIPanGestureRecognizer(target: context.coordinator, action: #selector(Coordinator.onPan1(_:)))
        pan1.minimumNumberOfTouches = 1
        pan1.maximumNumberOfTouches = 1
        v.addGestureRecognizer(pan1)

        let pan2 = UIPanGestureRecognizer(target: context.coordinator, action: #selector(Coordinator.onPan2(_:)))
        pan2.minimumNumberOfTouches = 2
        pan2.maximumNumberOfTouches = 2
        v.addGestureRecognizer(pan2)
        // Let a tap still register even though a pan is present.
        tap.require(toFail: pan1)
        return v
    }

    func updateUIView(_ uiView: UIView, context: Context) {
        context.coordinator.parent = self
    }

    final class Coordinator: NSObject {
        var parent: RemoteControlSurface
        private var oneFingerDown = false
        init(_ parent: RemoteControlSurface) { self.parent = parent }

        private func normalized(_ p: CGPoint, in view: UIView) -> CGPoint? {
            let frame = parent.frameSize ?? view.bounds.size
            return normalizedInVideo(touch: p, viewSize: view.bounds.size, frameSize: frame)
        }

        @objc func onTap(_ g: UITapGestureRecognizer) {
            guard let v = g.view, let n = normalized(g.location(in: v), in: v) else { return }
            parent.onEvent(.click(x: Double(n.x), y: Double(n.y), button: .left, count: 1))
        }

        @objc func onPan1(_ g: UIPanGestureRecognizer) {
            guard let v = g.view else { return }
            let loc = g.location(in: v)
            guard let n = normalized(loc, in: v) else {
                // Dragged into the letterbox: lift if we were holding.
                if oneFingerDown, g.state == .changed { return }
                return
            }
            let x = Double(n.x), y = Double(n.y)
            switch g.state {
            case .began:
                oneFingerDown = true
                parent.onEvent(.button(x: x, y: y, button: .left, down: true))
            case .changed:
                parent.onEvent(.move(x: x, y: y))
            case .ended, .cancelled, .failed:
                if oneFingerDown {
                    parent.onEvent(.button(x: x, y: y, button: .left, down: false))
                    oneFingerDown = false
                }
            default:
                break
            }
        }

        @objc func onPan2(_ g: UIPanGestureRecognizer) {
            guard let v = g.view, let n = normalized(g.location(in: v), in: v) else { return }
            let t = g.translation(in: v)
            // Scroll deltas in wheel "line" units; a small divisor keeps it from
            // being hypersensitive. Reset so each callback is an incremental delta.
            let dx = Double(t.x / 12.0)
            let dy = Double(t.y / 12.0)
            if abs(dx) >= 1 || abs(dy) >= 1 {
                parent.onEvent(.scroll(x: Double(n.x), y: Double(n.y), dx: dx, dy: dy))
                g.setTranslation(.zero, in: v)
            }
        }
    }
}
#endif
