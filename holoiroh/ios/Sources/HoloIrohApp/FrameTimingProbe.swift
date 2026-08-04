import Foundation
#if canImport(QuartzCore)
import QuartzCore
#endif

/// Measures frame timing while a debug probe changes video zoom and pan state.
/// The probe uses `CADisplayLink` to report these values:
/// - Frame count.
/// - Dropped-frame count.
/// - Average frame interval.
/// - Maximum frame interval.
///
/// The probe changes `MainView`'s persistent zoom and pan state.
/// It does not exercise gesture-only `@GestureState` values.
/// Use its results only for the persistent-state update path.
/// The probe runs only in debug builds when `HOLOIROH_FRAME_TIMING_PROBE=1`.
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
