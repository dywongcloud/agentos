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
    guard let n = videoRelativePoint(touch: touch, viewSize: viewSize, frameSize: frameSize) else {
        return nil
    }
    if n.x < 0 || n.x > 1 || n.y < 0 || n.y > 1 {
        return nil
    }
    return n
}

/// The same mapping, except a touch that strays into the letterbox slides along the nearest
/// video edge instead of vanishing.
///
/// Dropping those touches froze the cursor mid-drag: the aspect-fit bars sit exactly where a
/// thumb travels on a phone, and a wide desktop letterboxed into a short viewport puts them
/// within easy reach of any vertical drag. Sliding along the edge is also what a real trackpad
/// does when the pointer reaches the side of the screen, so it reads as continuous rather than
/// stuck. Returns `nil` only for degenerate sizes, where there is no video rect at all.
func normalizedInVideoClampedToEdges(touch: CGPoint, viewSize: CGSize, frameSize: CGSize) -> CGPoint? {
    guard let n = videoRelativePoint(touch: touch, viewSize: viewSize, frameSize: frameSize) else {
        return nil
    }
    return CGPoint(x: min(max(n.x, 0), 1), y: min(max(n.y, 0), 1))
}

/// The touch's position relative to the aspect-fit video rect, in `0..1` units of that rect --
/// outside `0..1` when the touch is in a letterbox bar. The one place the aspect-fit geometry
/// is computed, so the strict and clamped mappings can never disagree about where the image is.
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
    /// The live pinch-zoom the video underneath is rendered at.
    var zoom: CGFloat = 1
    /// The live pan the video underneath is rendered at.
    var pan: CGSize = .zero
    /// Sends one remote-control action over the control channel.
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
        /// Last touch point that was actually over the video image, in
        /// normalized coordinates. Used to release the mouse button when a drag
        /// ends out in the letterbox, where there is no valid current point —
        /// see `onPan1`.
        private var lastInVideo: CGPoint?
        /// The silent pinch detector, consulted by `onPan2` to suppress scroll
        /// while a pinch is in flight. Held weakly: the recognizer is owned by
        /// the view, which owns the coordinator.
        weak var pinch: UIPinchGestureRecognizer?
        /// The one pair that must NOT recognize together -- see
        /// `shouldRecognizeSimultaneouslyWith`. Weak for the same reason as `pinch`.
        weak var rightTap: UITapGestureRecognizer?
        weak var pan2: UIPanGestureRecognizer?
        init(_ parent: RemoteControlSurface) { self.parent = parent }

        /// Simultaneous recognition is allowed for every pair EXCEPT two-finger-tap against
        /// two-finger-scroll.
        ///
        /// The permissive default exists for a real reason -- see the doc on
        /// `[tap, rightTap, pan1, pan2, pinch].forEach { $0.delegate = ... }` in `makeUIView`:
        /// without it, UIKit treats these as mutually exclusive with recognizers on OTHER views
        /// and silently kills the SwiftUI `MagnificationGesture` this surface floats over. It is
        /// also what lets `onPan2` consult a live `pinch` state to suppress stray scroll, which
        /// only works because both recognize at once.
        ///
        /// But "allow everything" was too broad for the pair added with right-click. A two-finger
        /// tap that drifts past the pan threshold satisfies BOTH recognizers, and with simultaneous
        /// recognition enabled that is not an arbitration UIKit resolves -- both fire, sending a
        /// `.scroll` and a right `.click` for one gesture. Excluding exactly this pair leaves every
        /// other pairing, and the cross-view case, untouched.
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

        /// This surface is laid over the pan/zoom view but is NOT inside its scaled subtree, so
        /// UIKit hands us raw viewport coordinates while the video underneath is scaled and
        /// offset. Undoing that transform first is what makes aiming while zoomed land where the
        /// user is looking -- and what turns a zoomed view into finer cursor control.
        private func normalized(_ p: CGPoint, in view: UIView) -> CGPoint? {
            mapped(p, in: view, clampingToEdges: false)
        }

        /// The mapping used by the drag gestures, where a touch straying into the letterbox
        /// should slide the cursor along the video edge rather than freeze it.
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

        /// The remote cursor is the only signal that a tap registered, and it is a few hundred
        /// milliseconds of network and video encoding away. A tap that seems to do nothing gets
        /// tapped again, which is how a laggy link turns into accidental double-clicks. The
        /// haptic confirms locally and immediately that the tap was accepted and sent.
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

        /// Sends `count: 1`; the daemon derives the real click state from how soon and how near
        /// the previous click was, exactly as the window server does for a real mouse. Waiting
        /// out the double-click window here instead would put half a second of dead time on
        /// every single tap.
        @objc func onTwoFingerTap(_ g: UITapGestureRecognizer) {
            guard let v = g.view, let n = normalized(g.location(in: v), in: v) else { return }
            parent.onEvent(.click(x: Double(n.x), y: Double(n.y), button: .right, count: 1))
            confirmSent()
        }

        /// A real mouse/trackpad's secondary button, via `buttonMaskRequired = .secondary` on
        /// `pointerRightClick`. Separate handler from `onTwoFingerTap` (not a shared one) even
        /// though both ultimately send the same `.click(button: .right)`, so each recognizer's
        /// own input-source guarantee (finger vs. pointer device) stays explicit at the call
        /// site rather than folded into one function two different gestures happen to reach.
        @objc func onPointerRightClick(_ g: UITapGestureRecognizer) {
            guard let v = g.view, let n = normalized(g.location(in: v), in: v) else { return }
            parent.onEvent(.click(x: Double(n.x), y: Double(n.y), button: .right, count: 1))
            confirmSent()
        }

        /// Pure pointer movement from an attached trackpad/mouse -- no button involved, so this
        /// only ever sends `.move`, never `.button`. `UIHoverGestureRecognizer` fires
        /// continuously while an indirect pointer device moves over the view with nothing
        /// pressed; `onPan1` remains the only path that presses/releases the left button (a
        /// pointer device's actual click still arrives as a touch through `onPan1`/`onTap`, same
        /// as today -- iPadOS delivers a pointer click as a normal `.direct`-equivalent touch
        /// sequence, hover is purely the "aiming" motion in between).
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

        /// Translates one physical key press/release into a remote-control event, if this key
        /// is one this app forwards at all. Returns whether it was handled, so
        /// `RemoteControlInputView` can fall through to `super` for anything it doesn't
        /// recognize (an unhandled key should behave as if this view weren't intercepting
        /// presses at all, not silently vanish).
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

/// Backs `RemoteControlSurface`'s view: a plain `UIView` cannot become first responder or
/// receive `UIPress` events, and hardware-keyboard input only ever arrives at the first
/// responder. Becomes first responder as soon as it's placed in a window (remote control is
/// only ever shown while actively controlling, so there is no "sometimes wired up" state to
/// track) and forwards every `UIKey` press to the coordinator via `handleKeyPress`.
final class RemoteControlInputView: UIView {
    weak var coordinator: RemoteControlSurface.Coordinator?

    override var canBecomeFirstResponder: Bool { true }

    override func didMoveToWindow() {
        super.didMoveToWindow()
        if window != nil {
            becomeFirstResponder()
        }
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

    /// Maps a modifier key's own `UIKeyboardHIDUsage` to the daemon's held-modifier name
    /// (`remote_input.rs::key`'s `"cmd"`/`"ctrl"`/`"opt"`/`"shift"`), or `nil` for a non-modifier
    /// key. Left/right variants are folded together -- the daemon (and macOS shortcut handling
    /// generally) treats either side identically.
    static func modifierName(for keyCode: UIKeyboardHIDUsage) -> String? {
        switch keyCode {
        case .keyboardLeftGUI, .keyboardRightGUI: return "cmd"
        case .keyboardLeftControl, .keyboardRightControl: return "ctrl"
        case .keyboardLeftAlt, .keyboardRightAlt: return "opt"
        case .keyboardLeftShift, .keyboardRightShift: return "shift"
        default: return nil
        }
    }

    /// Maps a non-printable special key's `UIKeyboardHIDUsage` to the daemon's key name (the
    /// same table `remote_input.rs::keycode` accepts), or `nil` for anything with a printable
    /// character representation (those go through `charactersIgnoringModifiers`/`characters`
    /// instead, in `Coordinator.handleKeyPress`).
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
