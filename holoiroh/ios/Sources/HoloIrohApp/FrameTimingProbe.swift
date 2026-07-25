import Foundation
#if canImport(QuartzCore)
import QuartzCore
#endif

/// Debug-only empirical frame-timing witness for the video pan/zoom
/// performance investigation (the reported "panning and scrolling is choppy"
/// bug). `@GestureState` can only be mutated by a real `UIGestureRecognizer`
/// in flight, so this drives the plain `@State` `zoomScale`/`panOffset`
/// instead -- the only state a synthetic loop can reach without device
/// touch-automation tooling this session doesn't have. `CADisplayLink`
/// measures the real per-frame cost that drives, on real hardware.
///
/// IMPORTANT, live-witnessed caveat: `zoomScale`/`panOffset` are owned by
/// `MainView` in BOTH the pre- and post-`PanZoomVideoSurface` code (passed to
/// the child only as a `Binding`) -- `pinchScale`/`panDrag`, the
/// `@GestureState` a REAL pinch/pan actually ticks, is what moved into the
/// child. Driving `zoomScale`/`panOffset` therefore invalidates
/// `MainView.body` in both versions equally and never exercises the
/// per-View-struct `@GestureState` scoping the refactor optimizes -- a
/// before/after comparison using this harness measures a different, less
/// relevant code path (repeated `Binding` writes through an extra view
/// layer), not the live-touch-gesture win. Treat its numbers as raw data
/// about that path, not as confirmation either way of the real fix's effect;
/// see the `measure-after-refactor` PRD resolution for the full comparison
/// and why it came out the "wrong" direction for exactly this reason.
///
/// Gated behind `HOLOIROH_FRAME_TIMING_PROBE=1` (same convention as this
/// file's sibling `HOLOIROH_DEBUG_*` hooks in MainView.swift), `#if DEBUG`
/// only. Never active in a release build.
#if canImport(QuartzCore) && canImport(UIKit)
import UIKit

final class FrameTimingProbe {
    private var displayLink: CADisplayLink?
    private var lastTimestamp: CFTimeInterval?
    private var frameCount = 0
    private var droppedFrames = 0
    private var maxDeltaMs: Double = 0
    private var sumDeltaMs: Double = 0
    private let startedAt = CFAbsoluteTimeGetCurrent()
    private let label: String
    private let onFinish: (String) -> Void

    init(label: String, onFinish: @escaping (String) -> Void) {
        self.label = label
        self.onFinish = onFinish
    }

    func start() {
        let link = CADisplayLink(target: self, selector: #selector(tick))
        link.add(to: .main, forMode: .common)
        displayLink = link
        NSLog("FrameTimingProbe[\(label)]: started")
    }

    @objc private func tick(_ link: CADisplayLink) {
        let now = link.timestamp
        if let last = lastTimestamp {
            let deltaMs = (now - last) * 1000
            // Expected frame interval from the link's own actual/target
            // duration (correct on both 60Hz and ProMotion 120Hz devices,
            // rather than a hardcoded assumption). A frame is "dropped" if
            // its delta exceeds 1.5x that budget -- generous enough to not
            // flag ordinary scheduling jitter, tight enough to catch a real
            // missed-frame stall.
            let expectedMs = link.targetTimestamp > link.timestamp
                ? (link.targetTimestamp - link.timestamp) * 1000
                : 1000.0 / 60.0
            if deltaMs > expectedMs * 1.5 {
                droppedFrames += 1
            }
            maxDeltaMs = max(maxDeltaMs, deltaMs)
            sumDeltaMs += deltaMs
            frameCount += 1
        }
        lastTimestamp = now
    }

    func stop() {
        displayLink?.invalidate()
        displayLink = nil
        let avgMs = frameCount > 0 ? sumDeltaMs / Double(frameCount) : 0
        let summary = "FrameTimingProbe[\(label)]: frames=\(frameCount) dropped=\(droppedFrames) " +
            "avgDeltaMs=\(String(format: "%.2f", avgMs)) maxDeltaMs=\(String(format: "%.2f", maxDeltaMs))"
        NSLog(summary)
        onFinish(summary)
    }
}
#endif
