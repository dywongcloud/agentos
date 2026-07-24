import SwiftUI

/// The gesture-transform-affected slice of the live-share video: the
/// `VideoRenderView` itself, scaled/offset by the current pinch/pan, plus the
/// zoom badge that displays that same live scale.
///
/// Extracted out of `MainView.videoOverlay` (previously inline) to fix the
/// live-reported "panning and scrolling is choppy" performance bug: pinch and
/// pan gestures update `@GestureState` at touch-tracking frequency (up to
/// 60-120Hz on ProMotion devices). When those `@GestureState` properties lived
/// directly on `MainView` -- a single ~2200-line `View` whose `body` also
/// builds the orb background, task pills, command bar, and log panel --
/// SwiftUI had to re-evaluate and re-diff that ENTIRE tree on every single
/// gesture tick, not just the small video-transform subtree that actually
/// needed to change. SwiftUI's dependency tracking is scoped per-View-struct:
/// moving `pinchScale`/`panDrag` (and everything that reads them) into this
/// dedicated child means a gesture tick now only re-evaluates THIS struct's
/// own (much smaller) `body`, leaving `MainView.body` untouched.
///
/// `MainView.videoOverlay` still owns and chains on every OTHER overlay
/// (fullscreen toggle, RemoteControlSurface, remote-control toggle/banner,
/// remote-type bar) -- none of those depend on the live gesture scale/offset,
/// so they were never part of the problem and stay exactly where they were,
/// applied as modifiers on this struct's rendered output.
struct PanZoomVideoSurface: View {
    let frameSource: VideoFrameSource
    let viewport: CGSize
    let isVideoFullscreen: Bool
    let isControllingRemotely: Bool
    @Binding var zoomScale: CGFloat
    @Binding var panOffset: CGSize

    /// The pinch currently under the user's fingers (1 while none).
    @GestureState private var pinchScale: CGFloat = 1
    /// The drag currently under the user's finger (.zero while none).
    @GestureState private var panDrag: CGSize = .zero

    private static let zoomRange: ClosedRange<CGFloat> = 1...5

    private func clampZoom(_ value: CGFloat) -> CGFloat {
        min(max(value, Self.zoomRange.lowerBound), Self.zoomRange.upperBound)
    }

    /// Clamp a pan offset so the scaled content always covers the whole
    /// viewport (no revealed backdrop): with center-anchored scaling, the
    /// content edge reaches the viewport edge at +/- viewport*(scale-1)/2.
    private func clampedPan(_ proposed: CGSize, scale: CGFloat, viewport: CGSize) -> CGSize {
        let maxX = max(0, viewport.width * (scale - 1) / 2)
        let maxY = max(0, viewport.height * (scale - 1) / 2)
        return CGSize(
            width: min(max(proposed.width, -maxX), maxX),
            height: min(max(proposed.height, -maxY), maxY)
        )
    }

    private var liveScale: CGFloat {
        clampZoom(zoomScale * pinchScale)
    }

    private var liveOffset: CGSize {
        clampedPan(
            CGSize(width: panOffset.width + panDrag.width, height: panOffset.height + panDrag.height),
            scale: liveScale,
            viewport: viewport
        )
    }

    var body: some View {
        VideoRenderView(source: frameSource)
            .id(ObjectIdentifier(frameSource as AnyObject))
            .scaleEffect(liveScale)
            .offset(liveOffset)
            .frame(width: viewport.width, height: viewport.height)
            .background(Color.black)
            .clipShape(RoundedRectangle(cornerRadius: isVideoFullscreen ? 0 : 28))
            .overlay(
                RoundedRectangle(cornerRadius: isVideoFullscreen ? 0 : 28)
                    .stroke(Color.white.opacity(isVideoFullscreen ? 0 : 0.35), lineWidth: 1)
            )
            .overlay(alignment: .bottomLeading) {
                // Zoom badge, only while zoomed: current factor + an
                // affordance hint that double-tap resets.
                if liveScale > 1.01 {
                    HStack(spacing: 4) {
                        Text(String(format: "%.1f\u{00D7}", liveScale))
                            .font(.caption.weight(.semibold).monospacedDigit())
                        Image(systemName: "arrow.counterclockwise")
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }
                    .padding(.horizontal, 8)
                    .padding(.vertical, 4)
                    .background(.ultraThinMaterial, in: Capsule())
                    .overlay(Capsule().stroke(.white.opacity(0.12), lineWidth: 1))
                    .padding(10)
                    .transition(.scale(scale: 0.8).combined(with: .opacity))
                    .accessibilityLabel("Zoom \(String(format: "%.1f", liveScale))x, double tap to reset")
                }
            }
            .animation(.easeOut(duration: 0.18), value: liveScale > 1.01)
            // Pinch to zoom -- stays active even while controlling remotely:
            // pinch has no meaning as a remote-mouse action, so there's
            // nothing for it to conflict with. `RemoteControlSurface`'s own
            // pan2 (2-finger drag -> remote scroll) backs off during a
            // genuine pinch (see its `pinch.require` wiring in
            // RemoteControl.swift), so this can't misfire a stray remote
            // scroll either. `simultaneousGesture` so it composes with the
            // pan drag below and never blocks the taps MainView attaches
            // outside this struct.
            .simultaneousGesture(
                MagnificationGesture()
                    .updating($pinchScale) { value, state, _ in
                        state = value
                    }
                    .onEnded { value in
                        zoomScale = clampZoom(zoomScale * value)
                        panOffset = clampedPan(panOffset, scale: zoomScale, viewport: viewport)
                    }
            )
            // Drag to pan -- only once zoomed (at fit, the drag is ignored so
            // it can never swallow scroll-ish intents). minimumDistance keeps
            // single/double taps working.
            //
            // GATED OFF (GestureMask.none) while isControllingRemotely: a
            // 1-finger drag on the video during control is UNAMBIGUOUSLY
            // "move the remote cursor" (RemoteControlSurface's own pan1) --
            // this local viewport-pan gesture is a DIFFERENT interpretation
            // of the identical touch shape, and being a
            // `simultaneousGesture` (deliberately non-exclusive) it does not
            // lose the arbitration on its own.
            .simultaneousGesture(
                DragGesture(minimumDistance: 12)
                    .updating($panDrag) { value, state, _ in
                        guard zoomScale > 1.01 else { return }
                        state = value.translation
                    }
                    .onEnded { value in
                        guard zoomScale > 1.01 else { return }
                        panOffset = clampedPan(
                            CGSize(
                                width: panOffset.width + value.translation.width,
                                height: panOffset.height + value.translation.height
                            ),
                            scale: zoomScale,
                            viewport: viewport
                        )
                    },
                including: isControllingRemotely ? .none : .all
            )
    }
}
