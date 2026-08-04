import Foundation
import CoreMedia
import CoreVideo

// NOTE: This import is the C module produced from the `holoiroh-ios-bridge`
// Rust staticlib via `HoloirohIosBridge.xcframework` (see
// `holoiroh/ios-bridge/include/HoloirohIosBridge.h` + `module.modulemap`, and
// `holoiroh/ios/IROH_FFI.md`'s "As-built: xcframework packaging" section for
// the one build step -- link the xcframework -- a real Xcode project needs).
//
// It is behind `#if canImport(HoloirohIosBridge)` so this Swift Package
// keeps compiling for the iOS Simulator SDK WITHOUT the xcframework linked
// (the headless CI path used to witness the rest of the app), while the real
// app target that DOES link the xcframework gets the live implementation. The
// `#else` branch is a compile-time-honest stub that makes the missing-link
// state obvious at runtime rather than failing to build.
#if canImport(HoloirohIosBridge)
import HoloirohIosBridge
#endif

/// Receives decoded media stream frames through the bridge C interface.
///
/// The current app creates this source from `HoloConnection`'s connected bridge.
/// The source converts bridge-provided BGRA8 bytes to `CVPixelBuffer` values.
/// It sends each pixel buffer to `VideoRenderView` through `onFrame`.
///
/// A dedicated serial queue owns bridge calls and subscription polling.
/// The source invokes `onFrame` on that queue.
///
/// `HoloConnection` owns the bridge.
/// This source owns only its video subscription.
final class IrohLiveFrameSource: VideoFrameSource {
    var onFrame: ((VideoFrame) -> Void)?

    /// Protects frame time and size values across the poll and main threads.
    private let lastFrameLock = NSLock()
    private var _lastFrameAt: Date?
    private var _lastFrameSize: CGSize?
    var lastFrameAt: Date? {
        lastFrameLock.lock()
        defer { lastFrameLock.unlock() }
        return _lastFrameAt
    }
    var lastFrameSize: CGSize? {
        lastFrameLock.lock()
        defer { lastFrameLock.unlock() }
        return _lastFrameSize
    }
    private func markFrameDelivered(size: CGSize) {
        lastFrameLock.lock()
        _lastFrameAt = Date()
        _lastFrameSize = size
        lastFrameLock.unlock()
    }

    /// Serializes all bridge calls and subscription polling.
    private let queue = DispatchQueue(label: "com.holoiroh.irohlive.frame-source")

    /// Sets the delay after a poll reports no available frame.
    /// The delay is 1/60 second.
    private let pollInterval: TimeInterval = 1.0 / 60.0

    /// Protects the running flag across caller and poll threads.
    private let runLock = NSLock()
    private var isRunning = false

    /// Reads the running flag from any thread.
    private var running: Bool {
        runLock.lock()
        defer { runLock.unlock() }
        return isRunning
    }

    /// Sets the running flag and returns its previous value.
    @discardableResult
    private func setRunning(_ newValue: Bool) -> Bool {
        runLock.lock()
        defer { runLock.unlock() }
        let previous = isRunning
        isRunning = newValue
        return previous
    }

    /// Reuses a pixel buffer pool until the frame dimensions change.
    private var pixelBufferPool: CVPixelBufferPool?
    private var poolWidth: Int = 0
    private var poolHeight: Int = 0

    /// Holds bridge-provided BGRA8 bytes.
    /// The source grows this buffer when frame dimensions increase.
    private var scratch = [UInt8]()

    deinit {
        stop()
    }

    // MARK: - VideoFrameSource

    func start() {
        guard setRunning(true) == false else { return }
        queue.async { [weak self] in
            self?.runConnectAndPollLoop()
        }
    }

    func stop() {
        stop(completion: nil)
    }

    /// Stops polling and queues subscription cleanup.
    ///
    /// The running flag changes synchronously.
    /// Cleanup runs on the bridge queue before `completion`.
    /// A nonnil completion also drops the shared bridge pointer.
    /// A nil completion keeps that pointer for a later restart.
    /// Callers can use any thread.
    func stop(completion: (() -> Void)?) {
        setRunning(false)
        let ownerTeardown = completion != nil
        queue.async { [weak self] in
            self?.teardownHandles(releaseSharedBridge: ownerTeardown)
            completion?()
        }
    }

    // MARK: - FFI-backed implementation

    #if canImport(HoloirohIosBridge)

    private var bridge: OpaquePointer?
    private var subscription: OpaquePointer?

    /// Attaches the source to a connected bridge owned by `HoloConnection`.
    /// The source does not create, connect, or free this bridge.
    init(bridge: OpaquePointer) {
        self.bridge = bridge
    }

    /// Subscribes to video and polls until stopped or the track ends.
    /// Call this method only on `queue`.
    private func runConnectAndPollLoop() {
        guard let bridge else {
            NSLog("IrohLiveFrameSource: injected bridge is unavailable")
            setRunning(false)
            return
        }

        // Subscribe to the video track (blocks until a rendition appears).
        var subErr: UnsafeMutablePointer<CChar>?
        guard let subscription = holoiroh_ios_bridge_subscribe(bridge, &subErr) else {
            logFFIError("subscribe", status: -1, err: &subErr)
            // Keep the injected bridge on a subscribe failure: the connection
            // is still alive and a later view remount should get to retry.
            teardownHandles(releaseSharedBridge: false)
            setRunning(false)
            return
        }
        self.subscription = subscription

        // 4. Poll loop.
        pollLoop(subscription: subscription)
    }

    private func pollLoop(subscription: OpaquePointer) {
        while running {
            var frame = HoloirohFrame()
            // Ensure the scratch buffer can hold the current frame; on the very
            // first frame it is empty, so we take the BUFFER_TOO_SMALL path once
            // to learn the dimensions, then size it.
            let written: Int32 = scratch.withUnsafeMutableBufferPointer { buf in
                holoiroh_ios_bridge_poll_next_frame(
                    subscription,
                    buf.baseAddress,
                    UInt(buf.count),
                    &frame
                )
            }

            if written > 0 {
                deliverFrame(frame: frame, byteCount: Int(written))
            } else if written == 0 {
                // No frame yet -- wait a beat and poll again.
                Thread.sleep(forTimeInterval: pollInterval)
            } else if written == HOLOIROH_ERR_BUFFER_TOO_SMALL {
                // Grow the scratch buffer to the frame's real size and retry
                // immediately (metadata was filled even on this error path).
                let needed = Int(frame.width) * Int(frame.height) * 4
                if needed > scratch.count {
                    scratch = [UInt8](repeating: 0, count: needed)
                }
                // Loop around; next iteration copies into the resized buffer.
            } else if written == HOLOIROH_ERR_ENDED {
                NSLog("IrohLiveFrameSource: video track ended")
                break
            } else {
                NSLog("IrohLiveFrameSource: poll_next_frame error \(written)")
                Thread.sleep(forTimeInterval: pollInterval)
            }
        }
    }

    /// Copies one BGRA8 frame into a pooled pixel buffer.
    /// It drops invalid metadata, unsupported formats, and failed allocations.
    /// It sends successful frames through `onFrame` with immediate presentation timing.
    private func deliverFrame(frame: HoloirohFrame, byteCount: Int) {
        guard frame.pixel_format == UInt32(HOLOIROH_PIXFMT_BGRA8) else {
            NSLog("IrohLiveFrameSource: unexpected pixel_format \(frame.pixel_format)")
            return
        }
        let width = Int(frame.width)
        let height = Int(frame.height)
        guard width > 0, height > 0, byteCount >= width * height * 4 else { return }
        guard let handler = onFrame else { return }

        guard let pool = pixelBufferPool(width: width, height: height) else { return }
        var pbOut: CVPixelBuffer?
        guard CVPixelBufferPoolCreatePixelBuffer(kCFAllocatorDefault, pool, &pbOut) == kCVReturnSuccess,
              let pb = pbOut else { return }

        CVPixelBufferLockBaseAddress(pb, [])
        defer { CVPixelBufferUnlockBaseAddress(pb, []) }
        guard let base = CVPixelBufferGetBaseAddress(pb) else { return }

        let dstBytesPerRow = CVPixelBufferGetBytesPerRow(pb)
        let srcBytesPerRow = width * 4
        scratch.withUnsafeBufferPointer { src in
            guard let srcBase = src.baseAddress else { return }
            if dstBytesPerRow == srcBytesPerRow {
                memcpy(base, srcBase, height * srcBytesPerRow)
            } else {
                // Pool buffers may be row-padded; copy row by row.
                let dst = base.assumingMemoryBound(to: UInt8.self)
                for row in 0..<height {
                    memcpy(
                        dst.advanced(by: row * dstBytesPerRow),
                        srcBase.advanced(by: row * srcBytesPerRow),
                        srcBytesPerRow
                    )
                }
            }
        }

        // `.invalid` pts -> the render view tags it display-immediately (the
        // low-latency live-mirror path). The frame's own timestamp_us is
        // available on `frame` if in-order scheduling is ever wanted instead.
        markFrameDelivered(size: CGSize(
            width: CVPixelBufferGetWidth(pb), height: CVPixelBufferGetHeight(pb)))
        handler(.pixelBuffer(pb, pts: .invalid))
    }

    private func pixelBufferPool(width: Int, height: Int) -> CVPixelBufferPool? {
        if let pool = pixelBufferPool, poolWidth == width, poolHeight == height {
            return pool
        }
        let attrs: [String: Any] = [
            // 32BGRA: the only packed-RGBA-family format iOS CoreVideo
            // supports for IOSurface-backed pools (32RGBA silently fails
            // here on iOS -- see deliverFrame's doc for the black-screen
            // history behind this line).
            kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA,
            kCVPixelBufferWidthKey as String: width,
            kCVPixelBufferHeightKey as String: height,
            kCVPixelBufferIOSurfacePropertiesKey as String: [:] as [String: Any],
        ]
        var pool: CVPixelBufferPool?
        let status = CVPixelBufferPoolCreate(kCFAllocatorDefault, nil, attrs as CFDictionary, &pool)
        guard status == kCVReturnSuccess else {
            // LOUD, never silent: a failing pool here drops every frame with
            // no other symptom -- exactly how the unsupported-32RGBA-on-iOS
            // bug stayed invisible for an entire debugging campaign. If this
            // ever logs again, it is the same class of bug (unsupported
            // format / dimensions) recurring.
            NSLog("IrohLiveFrameSource: CVPixelBufferPoolCreate FAILED (\(status)) for \(width)x\(height) -- every frame will be dropped until this is fixed")
            return nil
        }
        pixelBufferPool = pool
        poolWidth = width
        poolHeight = height
        return pool
    }

    /// Frees the subscription.
    /// When requested, it also clears the borrowed bridge pointer.
    /// `HoloConnection` remains responsible for freeing the bridge.
    /// Repeated calls have no additional effect.
    private func teardownHandles(releaseSharedBridge: Bool) {
        if let subscription = subscription {
            holoiroh_ios_bridge_subscription_free(subscription)
            self.subscription = nil
        }
        if releaseSharedBridge {
            bridge = nil
        }
    }

    private func logFFIError(_ what: String, status: Int32, err: inout UnsafeMutablePointer<CChar>?) {
        if let err = err {
            NSLog("IrohLiveFrameSource: \(what) failed (\(status)): \(String(cString: err))")
            holoiroh_ios_bridge_free_error_string(err)
        } else {
            NSLog("IrohLiveFrameSource: \(what) failed (\(status))")
        }
        err = nil
    }

    #else

    // The xcframework is not linked (e.g. the headless SwiftPM/simulator build
    // used to witness the rest of the app). Compile-honest stub: it builds and
    // conforms, but produces no frames and logs why. The real app target links
    // `HoloirohIosBridge.xcframework` and gets the live implementation above.
    private func runConnectAndPollLoop() {
        NSLog(
            "IrohLiveFrameSource: HoloirohIosBridge not linked -- build the ios-bridge "
                + "xcframework and add it to the app target (see ios/IROH_FFI.md). "
                + "No live frames will be produced."
        )
        setRunning(false)
    }

    private func teardownHandles(releaseSharedBridge: Bool) {}

    #endif
}
