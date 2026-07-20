import Foundation
import CoreMedia
import CoreVideo
import CoreGraphics
import QuartzCore

/// A `VideoFrameSource` that draws animated frames **on device**, with no
/// network, so the render path (`VideoRenderView` +
/// `AVSampleBufferDisplayLayer`) can be exercised end-to-end before the
/// real `iroh-live` frame source exists.
///
/// This is a legitimate render witness, not a mock of the render path: the
/// frames are real `CVPixelBuffer`s, delivered through the exact same
/// `onFrame` seam a network source uses, and shown by the exact same
/// display layer. Only the *origin* of the pixels is synthetic. When the
/// `ios-bridge` subscribe/poll-next-frame path (see `ios/IROH_FFI.md`) is
/// wired up, a `IrohLiveFrameSource` conforming to `VideoFrameSource`
/// replaces this one at `MainView`'s binding site -- nothing in the view
/// changes.
///
/// ## What it draws
/// Each frame is a diagonal color gradient whose phase advances every
/// tick (so the whole field visibly scrolls) plus a bright vertical bar
/// that sweeps left-to-right, giving an unmistakable "this is live and
/// animating" motion cue that is trivial to verify by sampling two
/// consecutive frames' pixels.
///
/// ## Timing
/// Driven by a `CADisplayLink` at the display's refresh rate. Each frame
/// is stamped with a monotonically increasing presentation timestamp so
/// the display layer schedules them in order.
final class SyntheticVideoFrameSource: VideoFrameSource {
    var onFrame: ((VideoFrame) -> Void)?

    /// See `VideoFrameSource.lastFrameAt`. Timer-driven on the main thread,
    /// but locked anyway so the protocol's any-thread-read promise holds.
    private let lastFrameLock = NSLock()
    private var _lastFrameAt: Date?
    var lastFrameAt: Date? {
        lastFrameLock.lock()
        defer { lastFrameLock.unlock() }
        return _lastFrameAt
    }

    private let width: Int
    private let height: Int
    private let timescale: CMTimeScale = 600

    private var displayLink: CADisplayLink?
    private var pixelBufferPool: CVPixelBufferPool?
    private var frameIndex: Int64 = 0
    private var isRunning = false

    /// - Parameters:
    ///   - width/height: frame dimensions in pixels. Clamped to a sane
    ///     minimum so a degenerate `0x0` request cannot produce an
    ///     un-allocatable pool. 16:9 default approximates a desktop mirror.
    init(width: Int = 1280, height: Int = 720) {
        self.width = max(16, width)
        self.height = max(16, height)
    }

    deinit {
        stop()
    }

    // MARK: - VideoFrameSource

    func start() {
        guard !isRunning else { return }
        // Build the pool once; failure leaves the source inert (no frames)
        // rather than crashing -- a witness of the failure path, not a trap.
        guard makePixelBufferPoolIfNeeded() else { return }
        isRunning = true

        let link = CADisplayLink(target: self, selector: #selector(tick))
        // Add on the main run loop: CADisplayLink must be scheduled on a
        // run loop with an active display, which is the main one.
        link.add(to: .main, forMode: .common)
        displayLink = link
    }

    func stop() {
        isRunning = false
        displayLink?.invalidate()
        displayLink = nil
    }

    // MARK: - Frame production

    /// Called by `CADisplayLink` on the main thread once per refresh.
    /// Renders one frame and pushes it through `onFrame`.
    @objc private func tick(_ link: CADisplayLink) {
        guard isRunning, let handler = onFrame else { return }
        lastFrameLock.lock()
        _lastFrameAt = Date()
        lastFrameLock.unlock()
        guard let pixelBuffer = renderFrame(index: frameIndex) else { return }
        let pts = CMTime(value: frameIndex, timescale: timescale)
        frameIndex += 1
        handler(.pixelBuffer(pixelBuffer, pts: pts))
    }

    /// Render a single animated BGRA frame at the given index. Exposed
    /// (internal, not private) so the render path can be witnessed
    /// headlessly by pulling frames directly, without a display link.
    func renderFrame(index: Int64) -> CVPixelBuffer? {
        guard makePixelBufferPoolIfNeeded(), let pool = pixelBufferPool else { return nil }

        var pixelBufferOut: CVPixelBuffer?
        let status = CVPixelBufferPoolCreatePixelBuffer(kCFAllocatorDefault, pool, &pixelBufferOut)
        guard status == kCVReturnSuccess, let pixelBuffer = pixelBufferOut else {
            return nil
        }

        CVPixelBufferLockBaseAddress(pixelBuffer, [])
        defer { CVPixelBufferUnlockBaseAddress(pixelBuffer, []) }

        guard let base = CVPixelBufferGetBaseAddress(pixelBuffer) else { return nil }
        let bytesPerRow = CVPixelBufferGetBytesPerRow(pixelBuffer)
        let bufferWidth = CVPixelBufferGetWidth(pixelBuffer)
        let bufferHeight = CVPixelBufferGetHeight(pixelBuffer)

        // Animate: gradient phase scrolls with the frame index; a bright
        // vertical bar sweeps across so motion is obvious frame-to-frame.
        let phase = Int(index)
        let barX = (phase * 8) % max(1, bufferWidth)

        let ptr = base.assumingMemoryBound(to: UInt8.self)
        for y in 0..<bufferHeight {
            let row = ptr.advanced(by: y * bytesPerRow)
            for x in 0..<bufferWidth {
                let pixel = row.advanced(by: x * 4)
                // Diagonal gradient that shifts every frame.
                let r = UInt8((x + phase) & 0xFF)
                let g = UInt8((y + phase) & 0xFF)
                let b = UInt8((x + y + phase * 2) & 0xFF)
                let onBar = abs(x - barX) < 12
                // BGRA byte order (kCVPixelFormatType_32BGRA).
                pixel[0] = onBar ? 255 : b   // B
                pixel[1] = onBar ? 255 : g   // G
                pixel[2] = onBar ? 255 : r   // R
                pixel[3] = 255               // A
            }
        }

        return pixelBuffer
    }

    // MARK: - Pixel buffer pool

    /// Create the `CVPixelBufferPool` on first use. Reusing a pool across
    /// ticks avoids per-frame allocation churn. Returns `false` (leaving
    /// `pixelBufferPool == nil`) if allocation fails, so callers guard on
    /// it rather than assuming success.
    @discardableResult
    private func makePixelBufferPoolIfNeeded() -> Bool {
        if pixelBufferPool != nil { return true }

        let pixelBufferAttributes: [String: Any] = [
            kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA,
            kCVPixelBufferWidthKey as String: width,
            kCVPixelBufferHeightKey as String: height,
            kCVPixelBufferIOSurfacePropertiesKey as String: [:] as [String: Any],
        ]

        var pool: CVPixelBufferPool?
        let status = CVPixelBufferPoolCreate(
            kCFAllocatorDefault,
            nil,
            pixelBufferAttributes as CFDictionary,
            &pool
        )
        guard status == kCVReturnSuccess, let pool else {
            return false
        }
        pixelBufferPool = pool
        return true
    }
}
