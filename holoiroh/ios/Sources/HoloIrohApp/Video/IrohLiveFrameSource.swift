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
/// Two modes. **Owning** (`init(ticket:)`): `start()` creates the bridge,
/// connects the ticket, subscribes, and spins the poll loop; `stop()` tears
/// the loop down and frees the subscription then the bridge, in that order
/// (the subscription's decoder is driven by the bridge's runtime, so it must
/// be freed first). **Shared-bridge** (`init(bridge:)`, used by
/// `HoloConnection`): the caller owns an already-ticket-connected bridge that
/// also carries the control channel; this source only subscribes + polls, and
/// on `stop()` frees only the subscription -- the owner frees the bridge from
/// `stop(completion:)`'s completion. Idempotent both ways.
final class IrohLiveFrameSource: VideoFrameSource {
    var onFrame: ((VideoFrame) -> Void)?

    /// See `VideoFrameSource.lastFrameAt`. Written on the poll queue
    /// (`deliverFrame`), read from the main thread (foreground liveness
    /// check) -- hence the lock, not a bare stored property.
    private let lastFrameLock = NSLock()
    private var _lastFrameAt: Date?
    var lastFrameAt: Date? {
        lastFrameLock.lock()
        defer { lastFrameLock.unlock() }
        return _lastFrameAt
    }
    private func markFrameDelivered() {
        lastFrameLock.lock()
        _lastFrameAt = Date()
        lastFrameLock.unlock()
    }

    /// The `iroh-live:` ticket pasted/scanned on the pairing screen. `nil` in
    /// shared-bridge mode (`init(bridge:)`), where the caller has already
    /// ticket-connected the bridge it hands in.
    private let ticket: String?

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

    /// Protects `isRunning` so `stop()` can flip it synchronously from any
    /// thread while the poll loop reads it from `queue` -- the loop occupies
    /// the serial queue continuously, so an async-enqueued flip would never
    /// run until the loop exited on its own.
    private let runLock = NSLock()
    private var isRunning = false

    /// Thread-safe read of the running flag (the poll loop's exit condition).
    private var running: Bool {
        runLock.lock()
        defer { runLock.unlock() }
        return isRunning
    }

    /// Thread-safe set; returns the previous value (for start idempotence).
    @discardableResult
    private func setRunning(_ newValue: Bool) -> Bool {
        runLock.lock()
        defer { runLock.unlock() }
        let previous = isRunning
        isRunning = newValue
        return previous
    }

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
        guard setRunning(true) == false else { return }
        queue.async { [weak self] in
            self?.runConnectAndPollLoop()
        }
    }

    func stop() {
        stop(completion: nil)
    }

    /// Stops the poll loop and frees this source's handles, then runs
    /// `completion` on the FFI queue. The flag flip really is synchronous
    /// (`runLock`), so the in-flight loop iteration observes it and exits,
    /// letting the queued teardown run. `completion` lets a shared-bridge
    /// owner (`HoloConnection`) free the bridge only *after* the subscription
    /// is gone (free order matters). May be called from `deinit` or teardown
    /// on any thread.
    ///
    /// A non-nil `completion` marks this as the OWNER's final teardown (the
    /// bridge is about to be freed), so the shared bridge pointer is dropped
    /// too. A nil `completion` (the `stop()` overload -- what
    /// `VideoRenderView.dismantleUIView` calls on every view unmount) keeps
    /// the shared bridge pointer so a later `start()` (view REmount) can
    /// re-subscribe. Live-witnessed bug this split fixes: SwiftUI
    /// mounts/unmounts the video surface on every session-state change
    /// (`SessionState.showsRemoteView`), and the old
    /// unconditionally-pointer-dropping teardown meant the FIRST unmount
    /// permanently killed video -- every later mount logged
    /// "IrohLiveFrameSource: no ticket and no injected bridge" (captured
    /// live from the device console) and bailed, a black screen with no
    /// user-visible error for the rest of the session.
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

    /// `false` in shared-bridge mode (`init(bridge:)`): the bridge belongs to
    /// `HoloConnection`, so `teardownHandles` frees only the subscription and
    /// leaves the bridge for its owner to free (via `stop(completion:)`).
    private var ownsBridge = true

    /// Shared-bridge mode: attach to an already-ticket-connected bridge owned
    /// by the caller (`HoloConnection`), which also runs the control channel
    /// on it. This source only subscribes to the video track and polls
    /// frames; it never creates, connects, or frees the bridge.
    init(bridge: OpaquePointer) {
        self.ticket = nil
        self.bridge = bridge
        self.ownsBridge = false
    }

    /// Connect + subscribe + poll, all on `queue`. Runs until the running
    /// flag is cleared by `stop()` or the track ends.
    private func runConnectAndPollLoop() {
        if bridge == nil {
            // Owning mode (`init(ticket:)`): create + ticket-connect here.
            guard let ticket else {
                NSLog("IrohLiveFrameSource: no ticket and no injected bridge")
                setRunning(false)
                return
            }
            // 1. Create the bridge (runtime + endpoint + Live session).
            guard let created = holoiroh_ios_bridge_new() else {
                NSLog("IrohLiveFrameSource: holoiroh_ios_bridge_new returned null")
                setRunning(false)
                return
            }
            self.bridge = created

            // 2. Connect the ticket (blocks; we are on a background queue).
            var connectErr: UnsafeMutablePointer<CChar>?
            let connectStatus = ticket.withCString { cstr in
                holoiroh_ios_bridge_ticket_connect(created, cstr, &connectErr)
            }
            if connectStatus != HOLOIROH_OK {
                logFFIError("ticket_connect", status: connectStatus, err: &connectErr)
                // Owning mode only reaches here (shared mode never
                // ticket-connects); the flag is moot but `true` matches the
                // "this attempt's bridge is being discarded" intent.
                teardownHandles(releaseSharedBridge: true)
                setRunning(false)
                return
            }
        }
        guard let bridge else {
            setRunning(false)
            return
        }

        // 3. Subscribe to the video track (blocks until a rendition appears).
        var subErr: UnsafeMutablePointer<CChar>?
        guard let subscription = holoiroh_ios_bridge_subscribe(bridge, &subErr) else {
            logFFIError("subscribe", status: -1, err: &subErr)
            // Keep the shared bridge on a subscribe failure: the connection
            // is still alive and a later view remount should get to retry
            // (owning mode still frees its own bridge via `ownsBridge`).
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

    /// Wrap the BGRA8 bytes in `scratch[0..<byteCount]` in a pooled
    /// `CVPixelBuffer` and hand it to `onFrame`.
    ///
    /// BGRA, not RGBA: `kCVPixelFormatType_32RGBA` is not a supported
    /// CoreVideo pool format on iOS (it is on macOS), so the previous
    /// RGBA pipeline's `CVPixelBufferPoolCreate` returned nil and this
    /// function silently dropped EVERY frame -- the permanent black screen,
    /// with the on-device decoder verifiably running at 20-40fps the whole
    /// time. The Rust bridge now swizzles to BGRA at its single copy-out
    /// choke point and tags frames `HOLOIROH_PIXFMT_BGRA8`.
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
        markFrameDelivered()
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

    /// Free the subscription then (if owned) the bridge -- order matters: the
    /// subscription's decoder is driven by the bridge's runtime. Idempotent.
    ///
    /// `releaseSharedBridge` controls the shared-mode bridge pointer: `false`
    /// (view unmount via `stop()`) keeps it so a later `start()` can
    /// re-subscribe on the same live connection; `true` (owner teardown via
    /// `stop(completion:)`, or owning-mode always) drops it because the
    /// owner is about to free the bridge -- keeping it would leave a
    /// dangling pointer for a post-shutdown remount to crash on. See
    /// `stop(completion:)`'s doc for the live-witnessed black-screen bug
    /// behind this distinction.
    private func teardownHandles(releaseSharedBridge: Bool) {
        if let subscription = subscription {
            holoiroh_ios_bridge_subscription_free(subscription)
            self.subscription = nil
        }
        if let bridge = bridge {
            if ownsBridge {
                holoiroh_ios_bridge_free(bridge)
                self.bridge = nil
            } else if releaseSharedBridge {
                self.bridge = nil
            }
            // else: shared mode, view unmount -- keep the pointer for remount.
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
                + "No live frames will be produced. ticket=\(ticket ?? "<shared bridge>")"
        )
        setRunning(false)
    }

    private func teardownHandles(releaseSharedBridge: Bool) {}

    #endif
}
