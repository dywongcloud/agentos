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

/// A `VideoFrameSource` that pulls **real decoded frames** off an `iroh-live`
/// subscription via the `holoiroh-ios-bridge` C FFI and pushes them into
/// `VideoRenderView` as `CVPixelBuffer`s.
///
/// This is the network frame *source* the render half was built to accept:
/// it hits the exact same `onFrame`/`start`/`stop` seam
/// `SyntheticVideoFrameSource` does, so dropping it into `MainView`'s single
/// binding site (`frameSource`) turns the live-view surface from an on-device
/// animation into a mirror of the Mac's screen -- with no change to
/// `VideoRenderView` or `MainView`.
///
/// ## Pipeline (per frame)
/// 1. `holoiroh_ios_bridge_poll_next_frame` fills a reused buffer with
///    tightly-packed RGBA8 bytes (`width * height * 4`, `pixel_format ==
///    HOLOIROH_PIXFMT_RGBA8`) plus a `HoloirohFrame` metadata struct.
/// 2. We wrap those bytes in a `CVPixelBuffer` (`kCVPixelFormatType_32RGBA`,
///    `bytesPerRow == width * 4`) -- copying into a pooled buffer so the
///    pixel buffer outlives the Rust-owned scratch buffer.
/// 3. `onFrame(.pixelBuffer(pb, pts: .invalid))` hands it to the render view,
///    which shows it display-immediately (the low-latency live-mirror path).
///
/// ## Threading
/// All FFI connect/subscribe/poll calls run on a dedicated background
/// `DispatchQueue` (never the main thread -- `ticket_connect`/`subscribe`
/// block, and the poll loop is continuous). `onFrame` is therefore invoked
/// off the main thread, which `VideoRenderView.enqueue` explicitly tolerates.
///
/// ## Lifecycle / ownership
/// `start()` creates the bridge, connects the ticket, subscribes, and spins
/// the poll loop. `stop()` tears the loop down and frees the subscription then
/// the bridge, in that order (the subscription's decoder is driven by the
/// bridge's runtime, so it must be freed first). Idempotent both ways.
final class IrohLiveFrameSource: VideoFrameSource {
    var onFrame: ((VideoFrame) -> Void)?

    /// The `iroh-live:` ticket pasted/scanned on the pairing screen.
    private let ticket: String

    /// Serial queue owning every FFI call and the poll loop. Serial so the
    /// opaque handles are only ever touched from one thread (the single-owner
    /// contract the C ABI documents).
    private let queue = DispatchQueue(label: "com.holoiroh.irohlive.frame-source")

    /// How long to wait between polls when no frame is ready. The Mac daemon
    /// publishes at ~30-60fps; polling a little faster than that keeps latency
    /// low without busy-spinning. `try_recv` on the Rust side always returns
    /// the *latest* decoded frame (draining older ones), so an occasional slow
    /// poll drops stale frames rather than falling behind.
    private let pollInterval: TimeInterval = 1.0 / 60.0

    private var isRunning = false

    /// Reused CVPixelBuffer pool (sized on first frame, re-created if the
    /// stream's dimensions change). Avoids a per-frame allocation.
    private var pixelBufferPool: CVPixelBufferPool?
    private var poolWidth: Int = 0
    private var poolHeight: Int = 0

    /// Reused scratch buffer the Rust side fills with RGBA8 bytes. Grown to
    /// the frame size on demand (query-then-fill).
    private var scratch = [UInt8]()

    init(ticket: String) {
        self.ticket = ticket
    }

    deinit {
        stop()
    }

    // MARK: - VideoFrameSource

    func start() {
        queue.async { [weak self] in
            guard let self, !self.isRunning else { return }
            self.isRunning = true
            self.runConnectAndPollLoop()
        }
    }

    func stop() {
        // Flip the flag synchronously so an in-flight loop iteration exits, then
        // free handles on the queue. `stop()` may be called from `deinit` or
        // teardown on any thread.
        queue.async { [weak self] in
            guard let self else { return }
            self.isRunning = false
            self.teardownHandles()
        }
    }

    // MARK: - FFI-backed implementation

    #if canImport(HoloirohIosBridge)

    private var bridge: OpaquePointer?
    private var subscription: OpaquePointer?

    /// Connect + subscribe + poll, all on `queue`. Runs until `isRunning` is
    /// cleared by `stop()` or the track ends.
    private func runConnectAndPollLoop() {
        // 1. Create the bridge (runtime + endpoint + Live session).
        guard let bridge = holoiroh_ios_bridge_new() else {
            NSLog("IrohLiveFrameSource: holoiroh_ios_bridge_new returned null")
            isRunning = false
            return
        }
        self.bridge = bridge

        // 2. Connect the ticket (blocks; we are on a background queue).
        var connectErr: UnsafeMutablePointer<CChar>?
        let connectStatus = ticket.withCString { cstr in
            holoiroh_ios_bridge_ticket_connect(bridge, cstr, &connectErr)
        }
        if connectStatus != HOLOIROH_OK {
            logFFIError("ticket_connect", status: connectStatus, err: &connectErr)
            teardownHandles()
            isRunning = false
            return
        }

        // 3. Subscribe to the video track (blocks until a rendition appears).
        var subErr: UnsafeMutablePointer<CChar>?
        guard let subscription = holoiroh_ios_bridge_subscribe(bridge, &subErr) else {
            logFFIError("subscribe", status: -1, err: &subErr)
            teardownHandles()
            isRunning = false
            return
        }
        self.subscription = subscription

        // 4. Poll loop.
        pollLoop(subscription: subscription)
    }

    private func pollLoop(subscription: OpaquePointer) {
        while isRunning {
            var frame = HoloirohFrame()
            // Ensure the scratch buffer can hold the current frame; on the very
            // first frame it is empty, so we take the BUFFER_TOO_SMALL path once
            // to learn the dimensions, then size it.
            let written: Int32 = scratch.withUnsafeMutableBufferPointer { buf in
                holoiroh_ios_bridge_poll_next_frame(
                    subscription,
                    buf.baseAddress,
                    buf.count,
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

    /// Wrap the RGBA8 bytes in `scratch[0..<byteCount]` in a pooled
    /// `CVPixelBuffer` and hand it to `onFrame`.
    private func deliverFrame(frame: HoloirohFrame, byteCount: Int) {
        guard frame.pixel_format == UInt32(HOLOIROH_PIXFMT_RGBA8) else {
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
        handler(.pixelBuffer(pb, pts: .invalid))
    }

    private func pixelBufferPool(width: Int, height: Int) -> CVPixelBufferPool? {
        if let pool = pixelBufferPool, poolWidth == width, poolHeight == height {
            return pool
        }
        let attrs: [String: Any] = [
            kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32RGBA,
            kCVPixelBufferWidthKey as String: width,
            kCVPixelBufferHeightKey as String: height,
            kCVPixelBufferIOSurfacePropertiesKey as String: [:] as [String: Any],
        ]
        var pool: CVPixelBufferPool?
        guard CVPixelBufferPoolCreate(kCFAllocatorDefault, nil, attrs as CFDictionary, &pool) == kCVReturnSuccess else {
            return nil
        }
        pixelBufferPool = pool
        poolWidth = width
        poolHeight = height
        return pool
    }

    /// Free the subscription then the bridge (order matters: the subscription's
    /// decoder is driven by the bridge's runtime). Idempotent.
    private func teardownHandles() {
        if let subscription = subscription {
            holoiroh_ios_bridge_subscription_free(subscription)
            self.subscription = nil
        }
        if let bridge = bridge {
            holoiroh_ios_bridge_free(bridge)
            self.bridge = nil
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
                + "No live frames will be produced. ticket=\(ticket)"
        )
        isRunning = false
    }

    private func teardownHandles() {}

    #endif
}
