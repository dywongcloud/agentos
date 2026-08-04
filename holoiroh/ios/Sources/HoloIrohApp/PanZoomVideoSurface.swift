import SwiftUI

/// Renders the media stream with local pan and zoom gestures.
/// This view owns intermediate gesture state.
/// The parent owns committed pan and zoom values.
/// The parent applies controls and other overlays outside the transformed video.
/// `RemoteControlSurface` uses the same transform to map viewport coordinates.
struct PanZoomVideoSurface: View {
    let frameSource: VideoFrameSource
    let viewport: CGSize
    let isVideoFullscreen: Bool
    let isControllingRemotely: Bool
    @Binding var zoomScale: CGFloat
    @Binding var panOffset: CGSize

    /// Contains the active pinch scale.
    /// The value is `1` when no pinch is active.
    @GestureState private var pinchScale: CGFloat = 1
    /// Contains the active pan translation.
    /// The value is zero when no pan is active.
    @GestureState private var panDrag: CGSize = .zero

    private func clampZoom(_ value: CGFloat) -> CGFloat {
        VideoViewportTransform.clampZoom(value)
    }

    private func clampedPan(_ proposed: CGSize, scale: CGFloat, viewport: CGSize) -> CGSize {
        VideoViewportTransform.clampedPan(proposed, scale: scale, viewport: viewport)
    }

    /// Combines committed and active gesture values.
    /// `RemoteControlSurface` inverts this transform for coordinate mapping.
    private var liveTransform: VideoViewportTransform {
        VideoViewportTransform(
            zoom: zoomScale * pinchScale,
            pan: CGSize(width: panOffset.width + panDrag.width, height: panOffset.height + panDrag.height),
            viewport: viewport
        )
    }

    private var liveScale: CGFloat { liveTransform.scale }

    private var liveOffset: CGSize { liveTransform.offset }

    private func recordGestureWitness(_ kind: String) {
        #if DEBUG
        guard ProcessInfo.processInfo.environment["HOLOIROH_WITNESS_GESTURE_SURFACE"] == "1" else { return }
        NSLog(
            "HOLOIROH_GESTURE_WITNESS %@ zoom=%.3f pan_x=%.1f pan_y=%.1f",
            kind,
            zoomScale,
            panOffset.width,
            panOffset.height
        )
        #endif
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
                        recordGestureWitness("pinch")
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
                        recordGestureWitness("pan")
                    },
                including: isControllingRemotely ? .none : .all
            )
    }
}
