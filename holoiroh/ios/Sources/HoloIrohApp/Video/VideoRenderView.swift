import SwiftUI
import AVFoundation
import CoreMedia
import CoreVideo

/// Low-latency video surface: a SwiftUI wrapper around a UIKit view whose
/// backing layer is an `AVSampleBufferDisplayLayer`.
///
/// This is the *render* half of the live-view feature. It knows nothing
/// about iroh, networking, or decoding -- it only takes decoded frames
/// (`CVPixelBuffer` or `CMSampleBuffer`) and puts pixels on screen with
/// minimal latency. It binds to a `VideoFrameSource` (see
/// `VideoFrameSource.swift`), so the concrete producer -- the on-device
/// synthetic source today, the real `iroh-live` subscription later -- is
/// swappable without touching this view.
///
/// ## Why `AVSampleBufferDisplayLayer`
/// It is Apple's purpose-built surface for feeding a stream of
/// `CMSampleBuffer`s straight to the compositor. For a remote-screen
/// mirror the priorities are *low latency* and *not dropping frames*, not
/// A/V sync against an audio track -- so each frame is tagged
/// display-immediately and the layer shows it as soon as it is decoded,
/// with no reordering/lookahead buffer. This matches the Mac daemon's
/// `iroh-live` VideoToolbox-encoded H.264/HEVC stream (see
/// `holoiroh/mac-daemon/src/capture.rs`): the source-side decoder produces
/// `CVPixelBuffer`s that land here via `enqueue`.
///
/// ## Threading
/// `enqueue(_:)` (both overloads) is safe to call from **any** thread. It
/// hops to the layer's serial queue internally, because
/// `AVSampleBufferDisplayLayer` is not safe to mutate concurrently and a
/// real network/decode source delivers frames off the main thread.
struct VideoRenderView: UIViewRepresentable {
    /// The frame producer to bind to. The view starts it on appear and
    /// stops it on disappear, and routes its `onFrame` into `enqueue`.
    let source: VideoFrameSource

    /// How the video is fit into the view's bounds. `.resizeAspect`
    /// (letterbox, preserve aspect ratio) matches a desktop-capture mirror
    /// where distorting the image would be worse than black bars.
    var videoGravity: AVLayerVideoGravity = .resizeAspect

    func makeCoordinator() -> Coordinator {
        Coordinator()
    }

    func makeUIView(context: Context) -> SampleBufferView {
        let view = SampleBufferView()
        view.displayLayer.videoGravity = videoGravity
        view.backgroundColor = .black

        // Wire the source's frames into the layer. `[weak view]` so the
        // source's retained closure never keeps the view (and thus the
        // whole layer) alive past teardown.
        context.coordinator.view = view
        context.coordinator.source = source
        source.onFrame = { [weak view] frame in
            view?.enqueue(frame)
        }
        source.start()
        return view
    }

    func updateUIView(_ uiView: SampleBufferView, context: Context) {
        uiView.displayLayer.videoGravity = videoGravity
    }

    /// SwiftUI calls this when the representable is torn down. Stop the
    /// source and drop its closure so nothing keeps firing into a dead
    /// view, and flush the layer so a later reuse starts clean.
    static func dismantleUIView(_ uiView: SampleBufferView, coordinator: Coordinator) {
        coordinator.source?.stop()
        coordinator.source?.onFrame = nil
        coordinator.source = nil
        uiView.flush()
    }

    /// Holds the strong references SwiftUI's value-type `View` cannot, and
    /// gives `dismantleUIView` something to stop/detach.
    final class Coordinator {
        weak var view: SampleBufferView?
        var source: VideoFrameSource?
    }
}

/// A plain `UIView` whose backing layer *is* an
/// `AVSampleBufferDisplayLayer` (via the `layerClass` override, the
/// standard way to back a view with a specific `CALayer` subclass so the
/// layer is sized/positioned by the view automatically).
///
/// Kept as a distinct UIKit type (rather than an inline closure in the
/// representable) so the enqueue/flush/recovery logic has one clear home
/// and the thread-safety contract lives in one place.
final class SampleBufferView: UIView {
    override class var layerClass: AnyClass { AVSampleBufferDisplayLayer.self }

    /// Convenience typed accessor for the backing layer.
    var displayLayer: AVSampleBufferDisplayLayer {
        // Safe by construction: `layerClass` guarantees the backing layer's
        // type. A failure here would mean UIKit ignored `layerClass`, which
        // does not happen -- so a hard trap is the correct fail-fast.
        guard let layer = layer as? AVSampleBufferDisplayLayer else {
            fatalError("SampleBufferView.layer was not an AVSampleBufferDisplayLayer")
        }
        return layer
    }

    /// Serial queue that owns every mutation of `displayLayer`. `enqueue`
    /// hops here from whatever thread the frame arrived on, so the layer is
    /// never touched concurrently.
    private let layerQueue = DispatchQueue(label: "com.holoiroh.videorender.enqueue")

    // MARK: - Public enqueue API (thread-safe)

    /// Enqueue one `VideoFrame` from a `VideoFrameSource`. Safe from any
    /// thread.
    func enqueue(_ frame: VideoFrame) {
        switch frame {
        case let .pixelBuffer(pixelBuffer, pts):
            enqueue(pixelBuffer, pts: pts)
        case let .sampleBuffer(sampleBuffer):
            enqueue(sampleBuffer)
        }
    }

    /// Enqueue an already-formed `CMSampleBuffer` for display. Safe from
    /// any thread. This is the path a decoder that emits sample buffers
    /// (or a demuxer) would use directly.
    func enqueue(_ sampleBuffer: CMSampleBuffer) {
        layerQueue.async { [weak self] in
            guard let self else { return }
            self.recoverIfNeeded()
            let layer = self.displayLayer
            if #available(iOS 17.0, *) {
                // On iOS 17+ the renderer is the enqueue target and exposes
                // its own readiness; keep enqueuing while it accepts data.
                layer.sampleBufferRenderer.enqueue(sampleBuffer)
            } else {
                layer.enqueue(sampleBuffer)
            }
        }
    }

    /// Wrap a decoded `CVPixelBuffer` in a `CMSampleBuffer` (building a
    /// format description + timing) and enqueue it. Safe from any thread.
    ///
    /// `pts` drives scheduling; pass `.invalid` to show the frame
    /// immediately (a display-immediately attachment is added in that
    /// case). Building the sample buffer is done on the caller's thread
    /// (cheap, no layer access) and only the enqueue hops to `layerQueue`.
    func enqueue(_ pixelBuffer: CVPixelBuffer, pts: CMTime) {
        guard let sampleBuffer = Self.makeSampleBuffer(from: pixelBuffer, pts: pts) else {
            // Conversion failed (bad OSStatus). Drop this frame rather than
            // crash -- the next frame gets a fresh attempt.
            return
        }
        enqueue(sampleBuffer)
    }

    // MARK: - Recovery

    /// Recover the layer/renderer if it has entered `.failed` (e.g. after a
    /// decode error or returning from background) or explicitly asked to be
    /// flushed. Without this, one failure would silently swallow every
    /// subsequent frame for the life of the layer. Must run on `layerQueue`.
    private func recoverIfNeeded() {
        let layer = displayLayer
        if #available(iOS 17.0, *) {
            let renderer = layer.sampleBufferRenderer
            if renderer.status == .failed {
                renderer.flush()
            } else if renderer.requiresFlushToResumeDecoding {
                renderer.flush()
            }
        } else {
            if layer.status == .failed {
                layer.flush()
            } else if layer.requiresFlushToResumeDecoding {
                layer.flush()
            }
        }
    }

    /// Flush pending frames and clear the displayed image. Safe from any
    /// thread; used on teardown so a reused view starts blank.
    func flush() {
        layerQueue.async { [weak self] in
            guard let self else { return }
            let layer = self.displayLayer
            if #available(iOS 17.0, *) {
                layer.sampleBufferRenderer.flush()
            } else {
                layer.flushAndRemoveImage()
            }
        }
    }

    // MARK: - CVPixelBuffer -> CMSampleBuffer

    /// Build a display-ready `CMSampleBuffer` from a decoded pixel buffer.
    ///
    /// Returns `nil` (rather than trapping) on any failing `OSStatus`, so a
    /// single bad frame degrades to a dropped frame, not a crash.
    static func makeSampleBuffer(from pixelBuffer: CVPixelBuffer, pts: CMTime) -> CMSampleBuffer? {
        // Format description derived from the buffer itself -- the layer
        // needs it to know the frame's dimensions/pixel format.
        var formatDescription: CMVideoFormatDescription?
        let formatStatus = CMVideoFormatDescriptionCreateForImageBuffer(
            allocator: kCFAllocatorDefault,
            imageBuffer: pixelBuffer,
            formatDescriptionOut: &formatDescription
        )
        guard formatStatus == noErr, let formatDescription else {
            return nil
        }

        // A real presentation timestamp lets the layer schedule the frame;
        // a zero/invalid pts would make it drop or mis-order frames. When
        // the caller passes `.invalid` we substitute a valid-but-immediate
        // time and tag the buffer display-immediately below.
        let showImmediately = !pts.isValid
        let presentationTime = pts.isValid ? pts : CMTime(value: 0, timescale: 600)
        var timing = CMSampleTimingInfo(
            duration: .invalid,
            presentationTimeStamp: presentationTime,
            decodeTimeStamp: .invalid
        )

        var sampleBuffer: CMSampleBuffer?
        let sampleStatus = CMSampleBufferCreateReadyWithImageBuffer(
            allocator: kCFAllocatorDefault,
            imageBuffer: pixelBuffer,
            formatDescription: formatDescription,
            sampleTiming: &timing,
            sampleBufferOut: &sampleBuffer
        )
        guard sampleStatus == noErr, let sampleBuffer else {
            return nil
        }

        if showImmediately {
            // Ask the layer to present as soon as decoded, with no
            // reordering window -- the low-latency path for a live mirror.
            if let attachments = CMSampleBufferGetSampleAttachmentsArray(
                sampleBuffer,
                createIfNecessary: true
            ) as? [CFMutableDictionary], let first = attachments.first {
                CFDictionarySetValue(
                    first,
                    Unmanaged.passUnretained(kCMSampleAttachmentKey_DisplayImmediately).toOpaque(),
                    Unmanaged.passUnretained(kCFBooleanTrue).toOpaque()
                )
            }
        }

        return sampleBuffer
    }
}
