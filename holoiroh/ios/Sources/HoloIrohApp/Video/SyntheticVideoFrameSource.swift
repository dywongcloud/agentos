import Foundation
import CoreMedia
import CoreVideo
import CoreGraphics
import QuartzCore

/// Produces local animated frames for the video render path.
///
/// It sends real `CVPixelBuffer` values through the same interface as `IrohLiveFrameSource`.
/// It does not use the media stream.
/// The current app uses `IrohLiveFrameSource` for connected sessions.
final class SyntheticVideoFrameSource: VideoFrameSource {
    var onFrame: ((VideoFrame) -> Void)?

    /// Protects `lastFrameAt` for reads from any thread.
    private let lastFrameLock = NSLock()
    /// Reports the source's constant frame dimensions.
    var lastFrameSize: CGSize? { CGSize(width: width, height: height) }

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

    /// Creates a synthetic frame source.
    ///
    /// - Parameters:
    ///   - width: Requested width in pixels. Values below 16 become 16.
    ///   - height: Requested height in pixels. Values below 16 become 16.
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

        #if os(iOS)
        let link = CADisplayLink(target: self, selector: #selector(tick))
        // Add on the main run loop: CADisplayLink must be scheduled on a
        // run loop with an active display, which is the main one.
        link.add(to: .main, forMode: .common)
        displayLink = link
        #endif
    }

    func stop() {
        isRunning = false
        displayLink?.invalidate()
        displayLink = nil
    }

    // MARK: - Frame production

    /// Produces one frame for each display-link callback.
    /// The main run loop invokes this method.
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

    /// Creates one animated BGRA frame for the specified index.
    /// Returns `nil` if the pixel buffer cannot be created.
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

    /// Creates the pixel buffer pool when it does not exist.
    /// Returns `false` if Core Video cannot create the pool.
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
