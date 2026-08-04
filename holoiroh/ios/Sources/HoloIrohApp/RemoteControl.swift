import CoreGraphics
import Foundation

/// Mirrors the wire-format remote-control action.
/// The app sends this value inside `ClientMessage.remoteControl`.
/// Coordinate fields use normalized values from `0` through `1`.
/// The daemon maps normalized coordinates to display points.
enum RemoteControlEvent: Codable, Equatable {
    /// Takes hands-on control and pauses the active agent turn.
    case takeControl
    /// Releases hands-on control and resumes the paused agent turn.
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

/// Maps a viewport touch to normalized media-stream coordinates.
/// The function accounts for aspect-fit letterboxing.
/// It returns `nil` for touches outside the video image.
/// It also returns `nil` when a size is not positive.
func normalizedInVideo(touch: CGPoint, viewSize: CGSize, frameSize: CGSize) -> CGPoint? {
    guard let n = videoRelativePoint(touch: touch, viewSize: viewSize, frameSize: frameSize) else {
        return nil
    }
    if n.x < 0 || n.x > 1 || n.y < 0 || n.y > 1 {
        return nil
    }
    return n
}

/// Maps a viewport touch to normalized media-stream coordinates.
/// The function clamps letterbox touches to the nearest video edge.
/// It returns `nil` when a size is not positive.
func normalizedInVideoClampedToEdges(touch: CGPoint, viewSize: CGSize, frameSize: CGSize) -> CGPoint? {
    guard let n = videoRelativePoint(touch: touch, viewSize: viewSize, frameSize: frameSize) else {
        return nil
    }
    return CGPoint(x: min(max(n.x, 0), 1), y: min(max(n.y, 0), 1))
}

/// Calculates a touch position relative to the aspect-fit video rectangle.
/// Values outside `0...1` identify touches in letterbox bars.
/// It returns `nil` when a size is not positive.
func videoRelativePoint(touch: CGPoint, viewSize: CGSize, frameSize: CGSize) -> CGPoint? {
    guard viewSize.width > 0, viewSize.height > 0, frameSize.width > 0, frameSize.height > 0 else {
        return nil
    }
    let viewAspect = viewSize.width / viewSize.height
    let frameAspect = frameSize.width / frameSize.height
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
    return CGPoint(x: (touch.x - ox) / vw, y: (touch.y - oy) / vh)
}

#if canImport(UIKit)
import SwiftUI
import UIKit

/// Converts gestures over the media stream into remote-control actions.
///
/// The surface supports these gestures:
///
/// - A tap sends a primary click.
/// - A one-finger drag sends a primary-button drag.
/// - A two-finger tap sends a secondary click.
/// - A two-finger drag sends scroll deltas.
/// - Pointer hover sends cursor movement.
///
/// UIKit distinguishes the required touch counts and pointer input sources.
struct RemoteControlSurface: UIViewRepresentable {
    /// Provides the latest video-frame size for aspect-fit mapping.
    /// A `nil` value uses the viewport size.
    var frameSize: CGSize?
    /// Provides the current video zoom scale.
    var zoom: CGFloat = 1
    /// Provides the current video pan offset.
    var pan: CGSize = .zero
    /// Sends one action over the control channel.
    var onEvent: (RemoteControlEvent) -> Void

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeUIView(context: Context) -> UIView {
        let v = RemoteControlInputView()
        v.coordinator = context.coordinator
        v.backgroundColor = .clear
        v.isMultipleTouchEnabled = true

        let tap = UITapGestureRecognizer(target: context.coordinator, action: #selector(Coordinator.onTap(_:)))
        v.addGestureRecognizer(tap)

        // Two-finger tap -> right click.
        //
        // A long press would be the other obvious choice, but it fights the one-finger drag:
        // a user holding still before starting to drag would get a context menu they never
        // asked for. A two-finger tap needs no `require(toFail:)`, so it adds no latency to
        // anything.
        //
        // It does need one exclusion, and the first version of this comment got the reason
        // wrong: it argued that scroll requires translation and a tap has none, so "neither can
        // steal the other". Stealing was never the risk. This surface's delegate allows
        // simultaneous recognition, so a two-finger tap that drifts past the pan threshold
        // satisfies both and BOTH fire -- one gesture sending a scroll and a right click. See
        // `shouldRecognizeSimultaneouslyWith`, which excludes exactly this pair.
        let rightTap = UITapGestureRecognizer(target: context.coordinator, action: #selector(Coordinator.onTwoFingerTap(_:)))
        rightTap.numberOfTouchesRequired = 2
        v.addGestureRecognizer(rightTap)

        let pan1 = UIPanGestureRecognizer(target: context.coordinator, action: #selector(Coordinator.onPan1(_:)))
        pan1.minimumNumberOfTouches = 1
        pan1.maximumNumberOfTouches = 1
        v.addGestureRecognizer(pan1)

        let pan2 = UIPanGestureRecognizer(target: context.coordinator, action: #selector(Coordinator.onPan2(_:)))
        pan2.minimumNumberOfTouches = 2
        pan2.maximumNumberOfTouches = 2
        v.addGestureRecognizer(pan2)

        // Real trackpad/mouse pointer movement WITHOUT a button held. `pan1` only fires once a
        // touch is actually down, which for a finger is correct (there is no such thing as
        // "hovering" a finger) but for an attached pointer device is wrong: moving the mouse to
        // aim before clicking should move the remote cursor, exactly like a real Mac, not be
        // silently dropped until the button is pressed. `UIHoverGestureRecognizer` only ever
        // fires for indirect-pointer input (trackpad/mouse) -- a finger touch never triggers it
        // at all, so this is purely additive and cannot conflict with `pan1`'s touch handling.
        let hover = UIHoverGestureRecognizer(target: context.coordinator, action: #selector(Coordinator.onHover(_:)))
        v.addGestureRecognizer(hover)

        // Real secondary-click (right mouse button / two-finger trackpad click) in ADDITION to
        // the touch two-finger-tap above. `buttonMaskRequired` only matches an actual pointer
        // device reporting its secondary button, so this never fires from a finger tap and never
        // double-sends alongside `rightTap`.
        let pointerRightClick = UITapGestureRecognizer(target: context.coordinator, action: #selector(Coordinator.onPointerRightClick(_:)))
        pointerRightClick.buttonMaskRequired = .secondary
        pointerRightClick.allowedTouchTypes = [NSNumber(value: UITouch.TouchType.indirectPointer.rawValue)]
        v.addGestureRecognizer(pointerRightClick)

        // Silent pinch-detector: never sends anything itself (the LOCAL
        // MagnificationGesture elsewhere in the hierarchy owns the actual visual
        // zoom) -- it exists solely so `pan2.require(toFail:)` can make the
        // 2-finger-drag-to-scroll recognizer back off while the touches are
        // actually a pinch. Without this, a real pinch's small translational
        // component (two fingers moving apart/together are rarely perfectly
        // symmetric) was ALSO recognized by `pan2` as a pan, sending stray
        // `.scroll` events to the remote Mac on every local zoom gesture --
        // part of the live-reported "zoom in and out... accidentally... clicks
        // things" bug (the other part was the pan1-vs-local-pan conflict, fixed
        // in MainView's gesture gating).
        let pinch = UIPinchGestureRecognizer(target: context.coordinator, action: #selector(Coordinator.onPinchNoop(_:)))
        v.addGestureRecognizer(pinch)
        // Deliberately NOT `pan2.require(toFail: pinch)`.
        //
        // That dependency made two-finger scroll -- the only `.scroll` producer
        // in the app, and the most-used remote-desktop interaction after
        // clicking -- impossible to ever fire. `require(toFail:)` holds the
        // dependent in `.possible` and fails it outright once the prerequisite
        // reaches `.began`, and a two-touch UIPinchGestureRecognizer has no
        // meaningful scale deadband, so it begins almost immediately on any
        // two-finger gesture. Under either branch of the UIKit state machine
        // pan2 never delivered a usable scroll, and there was no fallback at the time this
        // comment was written -- `RemoteControlInputView`'s hardware-keyboard capture now
        // covers Page Down via a real `.key` event instead.
        //
        // The stray-scroll-during-pinch problem the dependency was meant to
        // solve is instead handled in `onPan2` by consulting this recognizer's
        // live state, which suppresses emission without suppressing the
        // recognizer itself.
        context.coordinator.pinch = pinch
        // Let a tap still register even though a pan is present.
        tap.require(toFail: pan1)

        // Without an explicit delegate, UIKit's default arbitration treats
        // every recognizer here as MUTUALLY EXCLUSIVE with recognizers on any
        // OTHER view -- including the SwiftUI `MagnificationGesture` this
        // surface floats directly on top of (`PanZoomVideoSurface`, a
        // separate SwiftUI-hosted UIView). Live-reported: pinch-to-zoom
        // simply stopped responding while in remote control -- `pinch` here
        // (added only so `pan2.require(toFail:)` can defer to it, see its own
        // doc) was silently WINNING that exclusivity race against the real
        // zoom gesture underneath, since nothing told UIKit the two are
        // allowed to recognize at once. `shouldRecognizeSimultaneouslyWith`
        // is independent of `require(toFail:)` -- allowing simultaneous
        // recognition here doesn't change pan2 still waiting on pinch to
        // fail; it only stops these four recognizers from blocking whatever
        // else is touching the screen underneath.
        context.coordinator.rightTap = rightTap
        context.coordinator.pan2 = pan2
        [tap, rightTap, pan1, pan2, pinch, hover, pointerRightClick].forEach { $0.delegate = context.coordinator }
        context.coordinator.prepareHaptics()
        return v
    }

    func updateUIView(_ uiView: UIView, context: Context) {
        context.coordinator.parent = self
    }

    final class Coordinator: NSObject, UIGestureRecognizerDelegate {
        var parent: RemoteControlSurface
        private var oneFingerDown = false
        /// Stores the last normalized point for a primary-button release.
        /// The drag can end where no current point is valid.
        private var lastInVideo: CGPoint?
        /// Detects an active pinch before `onPan2` emits scroll events.
        /// The view owns this recognizer.
        weak var pinch: UIPinchGestureRecognizer?
        /// Identifies the two-finger tap recognizer used in gesture arbitration.
        weak var rightTap: UITapGestureRecognizer?
        weak var pan2: UIPanGestureRecognizer?
        init(_ parent: RemoteControlSurface) { self.parent = parent }

        /// Allows simultaneous gesture recognition except between two-finger tap and scroll.
        /// This exception prevents one gesture from sending both actions.
        /// Cross-view recognition remains enabled for the SwiftUI magnification gesture.
        func gestureRecognizer(
            _ gestureRecognizer: UIGestureRecognizer,
            shouldRecognizeSimultaneouslyWith otherGestureRecognizer: UIGestureRecognizer
        ) -> Bool {
            !isTwoFingerTapVersusScroll(gestureRecognizer, otherGestureRecognizer)
        }

        private func isTwoFingerTapVersusScroll(
            _ a: UIGestureRecognizer,
            _ b: UIGestureRecognizer
        ) -> Bool {
            guard let rightTap, let pan2 else { return false }
            return (a === rightTap && b === pan2) || (a === pan2 && b === rightTap)
        }

        /// Converts a viewport point to strict normalized video coordinates.
        /// The remote-control surface remains outside the transformed video subtree.
        /// This function first removes the current video transform.
        private func normalized(_ p: CGPoint, in view: UIView) -> CGPoint? {
            mapped(p, in: view, clampingToEdges: false)
        }

        /// Converts a drag point to normalized video coordinates.
        /// The function clamps letterbox touches to the nearest video edge.
        private func normalizedClamped(_ p: CGPoint, in view: UIView) -> CGPoint? {
            mapped(p, in: view, clampingToEdges: true)
        }

        private func mapped(_ p: CGPoint, in view: UIView, clampingToEdges: Bool) -> CGPoint? {
            let viewport = view.bounds.size
            let frame = parent.frameSize ?? viewport
            let transform = VideoViewportTransform(zoom: parent.zoom, pan: parent.pan, viewport: viewport)
            let inContent = transform.viewportPointToContent(p, viewport: viewport)
            if clampingToEdges {
                return normalizedInVideoClampedToEdges(touch: inContent, viewSize: viewport, frameSize: frame)
            }
            return normalizedInVideo(touch: inContent, viewSize: viewport, frameSize: frame)
        }

        // Intentionally does nothing -- see `pinch`'s doc in `makeUIView`. This
        // recognizer exists only to make `pan2` back off during a real pinch;
        // the visual zoom itself is owned entirely by the LOCAL
        // MagnificationGesture elsewhere in the view hierarchy.
        @objc func onPinchNoop(_ g: UIPinchGestureRecognizer) {}

        /// Provides immediate local confirmation after the app sends a click.
        /// Remote visual feedback can arrive later through the media stream.
        private let clickHaptics = UIImpactFeedbackGenerator(style: .light)

        func prepareHaptics() {
            clickHaptics.prepare()
        }

        private func confirmSent() {
            clickHaptics.impactOccurred()
            clickHaptics.prepare()
        }

        @objc func onTap(_ g: UITapGestureRecognizer) {
            guard let v = g.view, let n = normalized(g.location(in: v), in: v) else { return }
            parent.onEvent(.click(x: Double(n.x), y: Double(n.y), button: .left, count: 1))
            confirmSent()
        }

        /// Sends a secondary click with `count` set to `1`.
        /// The daemon derives repeated-click state from timing and position.
        @objc func onTwoFingerTap(_ g: UITapGestureRecognizer) {
            guard let v = g.view, let n = normalized(g.location(in: v), in: v) else { return }
            parent.onEvent(.click(x: Double(n.x), y: Double(n.y), button: .right, count: 1))
            confirmSent()
        }

        /// Handles a secondary click from a physical pointer device.
        /// `pointerRightClick` restricts this handler to the secondary button.
        /// Two-finger touch taps use `onTwoFingerTap`.
        @objc func onPointerRightClick(_ g: UITapGestureRecognizer) {
            guard let v = g.view, let n = normalized(g.location(in: v), in: v) else { return }
            parent.onEvent(.click(x: Double(n.x), y: Double(n.y), button: .right, count: 1))
            confirmSent()
        }

        /// Sends pointer movement without a button action.
        /// `UIHoverGestureRecognizer` supplies events from an indirect pointer device.
        /// Primary-button actions remain in the tap and one-finger pan handlers.
        @objc func onHover(_ g: UIHoverGestureRecognizer) {
            guard let v = g.view, let n = normalized(g.location(in: v), in: v) else { return }
            switch g.state {
            case .began, .changed:
                parent.onEvent(.move(x: Double(n.x), y: Double(n.y)))
            default:
                break
            }
        }

        @objc func onPan1(_ g: UIPanGestureRecognizer) {
            guard let v = g.view else { return }
            let loc = g.location(in: v)

            // A letterbox touch now maps to the nearest video edge rather than to nothing, so
            // this only fires for a degenerate view/frame size. A terminal state must STILL
            // release: ending a drag with no released button leaves the Mac's left mouse button
            // physically held down, with no touch left on screen to lift it, selecting and
            // dragging everything the pointer crosses until someone uses the machine directly.
            guard let n = normalizedClamped(loc, in: v) else {
                if oneFingerDown, g.state == .ended || g.state == .cancelled || g.state == .failed {
                    let p = lastInVideo ?? CGPoint(x: 0.5, y: 0.5)
                    parent.onEvent(
                        .button(x: Double(p.x), y: Double(p.y), button: .left, down: false)
                    )
                    oneFingerDown = false
                }
                return
            }

            lastInVideo = n
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
            guard let v = g.view, let n = normalizedClamped(g.location(in: v), in: v) else { return }

            // Suppress scroll while the touches are actually a pinch. Two fingers
            // moving apart are rarely perfectly symmetric, so a real pinch carries
            // a translational component that would otherwise be sent to the Mac as
            // stray scrolling.
            //
            // The translation MUST still be zeroed on the suppressed path. Only
            // the emit branch below used to reset it, so letting it accumulate
            // during a pinch would flush as one large stale scroll the instant
            // suppression lifted -- reintroducing the exact bug this suppression
            // exists to prevent.
            if let pinch, pinch.state == .began || pinch.state == .changed {
                g.setTranslation(.zero, in: v)
                return
            }

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

        /// Converts one physical key transition to a remote-control event.
        /// It returns `true` when the app forwards the key.
        /// It returns `false` when normal responder handling must continue.
        @discardableResult
        func handleKeyPress(_ key: UIKey, down: Bool) -> Bool {
            if let modifierName = RemoteControlInputView.modifierName(for: key.keyCode) {
                parent.onEvent(.key(key: modifierName, down: down))
                return true
            }
            if let specialName = RemoteControlInputView.specialKeyName(for: key.keyCode) {
                parent.onEvent(.key(key: specialName, down: down))
                return true
            }
            // A real keyboard SHORTCUT (any of Cmd/Ctrl/Option held) has to go through `.key`
            // with the daemon's own held-modifier state applying `CGEventFlags` -- `.text`
            // injects a literal unicode string that bypasses shortcut interpretation entirely
            // (see remote_input.rs's `key()` doc). `charactersIgnoringModifiers` gives the base
            // unshifted character, which already matches the daemon's a-z/0-9/punctuation key
            // names directly -- no separate HID-usage-to-letter table needed.
            let shortcutModifiers: UIKeyModifierFlags = [.command, .control, .alternate]
            if !key.modifierFlags.intersection(shortcutModifiers).isEmpty {
                let name = key.charactersIgnoringModifiers.lowercased()
                guard !name.isEmpty else { return false }
                parent.onEvent(.key(key: name, down: down))
                return true
            }
            // Plain typing (no shortcut modifier -- Shift-for-capitals is already reflected in
            // `characters`). Sent once, on key-down only: `.text` injects a self-contained
            // down+up pair (see `remote_input.rs::text`'s doc), so a key-up here would double it.
            guard down, !key.characters.isEmpty else { return false }
            parent.onEvent(.text(key.characters))
            return true
        }
    }
}

/// Receives hardware-keyboard presses for `RemoteControlSurface`.
/// The view becomes first responder when it enters a window.
/// It forwards supported keys through `Coordinator.handleKeyPress`.
final class RemoteControlInputView: UIView {
    weak var coordinator: RemoteControlSurface.Coordinator?

    override var canBecomeFirstResponder: Bool { true }

    override func didMoveToWindow() {
        super.didMoveToWindow()
        if window != nil {
            becomeFirstResponder()
        }
    }

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
        becomeFirstResponder()
        super.touchesBegan(touches, with: event)
    }

    override func pressesBegan(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        let handled = presses.compactMap(\.key).reduce(false) { handledSoFar, key in
            coordinator?.handleKeyPress(key, down: true) == true || handledSoFar
        }
        if !handled {
            super.pressesBegan(presses, with: event)
        }
    }

    override func pressesEnded(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        let handled = presses.compactMap(\.key).reduce(false) { handledSoFar, key in
            coordinator?.handleKeyPress(key, down: false) == true || handledSoFar
        }
        if !handled {
            super.pressesEnded(presses, with: event)
        }
    }

    override func pressesCancelled(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        // Same terminal-state discipline as `onPan1`'s letterbox-release case: a press that gets
        // cancelled (e.g. the app backgrounds mid-keystroke) must still release, or a synthetic
        // key/modifier is left "held" with nothing left on screen to release it.
        presses.compactMap(\.key).forEach { coordinator?.handleKeyPress($0, down: false) }
        super.pressesCancelled(presses, with: event)
    }

    /// Maps a modifier usage to the daemon's held-modifier name.
    /// It combines left and right variants.
    /// It returns `nil` for other keys.
    static func modifierName(for keyCode: UIKeyboardHIDUsage) -> String? {
        switch keyCode {
        case .keyboardLeftGUI, .keyboardRightGUI: return "cmd"
        case .keyboardLeftControl, .keyboardRightControl: return "ctrl"
        case .keyboardLeftAlt, .keyboardRightAlt: return "opt"
        case .keyboardLeftShift, .keyboardRightShift: return "shift"
        default: return nil
        }
    }

    /// Maps a nonprinting key usage to the daemon's key name.
    /// It returns `nil` for keys that use a character representation.
    static func specialKeyName(for keyCode: UIKeyboardHIDUsage) -> String? {
        switch keyCode {
        case .keyboardEscape: return "escape"
        case .keyboardTab: return "tab"
        case .keyboardReturnOrEnter: return "return"
        case .keyboardDeleteOrBackspace: return "delete"
        case .keyboardDeleteForward: return "forwarddelete"
        case .keyboardSpacebar: return "space"
        case .keyboardLeftArrow: return "left"
        case .keyboardRightArrow: return "right"
        case .keyboardUpArrow: return "up"
        case .keyboardDownArrow: return "down"
        case .keyboardHome: return "home"
        case .keyboardEnd: return "end"
        case .keyboardPageUp: return "pageup"
        case .keyboardPageDown: return "pagedown"
        case .keyboardF1: return "f1"
        case .keyboardF2: return "f2"
        case .keyboardF3: return "f3"
        case .keyboardF4: return "f4"
        case .keyboardF5: return "f5"
        case .keyboardF6: return "f6"
        case .keyboardF7: return "f7"
        case .keyboardF8: return "f8"
        case .keyboardF9: return "f9"
        case .keyboardF10: return "f10"
        case .keyboardF11: return "f11"
        case .keyboardF12: return "f12"
        default: return nil
        }
    }
}
#endif
