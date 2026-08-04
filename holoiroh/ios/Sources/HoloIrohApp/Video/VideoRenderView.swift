import SwiftUI
import AVFoundation
import CoreMedia
import CoreVideo
#if canImport(UIKit)
import UIKit
#endif

/// Provides a SwiftUI video surface backed by `AVSampleBufferDisplayLayer`.
///
/// It accepts decoded frames from a `VideoFrameSource`.
/// `IrohLiveFrameSource` supplies media stream frames in the current app.
/// `SyntheticVideoFrameSource` supplies local diagnostic frames.
///
/// Sources can deliver frames from any thread.
/// The view serializes display-layer changes.
#if canImport(UIKit)
struct VideoRenderView: UIViewRepresentable {
    /// Frame source for this view.
    /// The view starts the source during creation and stops it during teardown.
    let source: VideoFrameSource

    /// Controls how video fits the view bounds.
    /// The default preserves the aspect ratio and can add letterboxing.
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

    /// Stops frame delivery when SwiftUI removes the view.
    /// It also clears the callback and flushes the display layer.
    static func dismantleUIView(_ uiView: SampleBufferView, coordinator: Coordinator) {
        coordinator.source?.stop()
        coordinator.source?.onFrame = nil
        coordinator.source = nil
        uiView.flush()
    }

    /// Retains the source and view references required during teardown.
    final class Coordinator {
        weak var view: SampleBufferView?
        var source: VideoFrameSource?
    }
}

/// Provides a `UIView` backed by `AVSampleBufferDisplayLayer`.
/// The view owns frame enqueue, flush, and recovery operations.
final class SampleBufferView: UIView {
    override class var layerClass: AnyClass { AVSampleBufferDisplayLayer.self }

    /// Provides typed access to the backing display layer.
    private(set) lazy var displayLayer: AVSampleBufferDisplayLayer = {
        // Safe by construction: `layerClass` guarantees the backing layer's
        // type. The first access is in `makeUIView` on the main thread. Later
        // enqueue operations reuse this layer reference without reading the
        // UIView's `layer` property from their worker queue.
        guard let layer = layer as? AVSampleBufferDisplayLayer else {
            fatalError("SampleBufferView.layer was not an AVSampleBufferDisplayLayer")
        }
        return layer
    }()

    /// Serializes all display-layer changes.
    private let layerQueue = DispatchQueue(label: "com.holoiroh.videorender.enqueue")

    // MARK: - Public enqueue API (thread-safe)

    /// Enqueues one decoded frame.
    /// Callers can use any thread.
    func enqueue(_ frame: VideoFrame) {
        switch frame {
        case let .pixelBuffer(pixelBuffer, pts):
            enqueue(pixelBuffer, pts: pts)
        case let .sampleBuffer(sampleBuffer):
            enqueue(sampleBuffer)
        }
    }

    /// Enqueues an existing sample buffer.
    /// Callers can use any thread.
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

    /// Converts a decoded pixel buffer to a sample buffer and enqueues it.
    /// Callers can use any thread.
    /// The method drops the frame if conversion fails.
    func enqueue(_ pixelBuffer: CVPixelBuffer, pts: CMTime) {
        guard let sampleBuffer = Self.makeSampleBuffer(from: pixelBuffer, pts: pts) else {
            // Conversion failed (bad OSStatus). Drop this frame rather than
            // crash -- the next frame gets a fresh attempt.
            return
        }
        enqueue(sampleBuffer)
    }

    // MARK: - Recovery

    /// Flushes a failed renderer before the next enqueue operation.
    /// Call this method only on `layerQueue`.
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

    /// Flushes pending frames and clears the displayed image.
    /// Callers can use any thread.
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

    /// Creates a display-ready sample buffer from a decoded pixel buffer.
    /// Returns `nil` if Core Media returns a failing status.
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
#else
struct VideoRenderView: View {
    let source: VideoFrameSource
    var videoGravity: AVLayerVideoGravity = .resizeAspect

    var body: some View {
        Color.black
            .onAppear { source.start() }
            .onDisappear {
                source.stop()
                source.onFrame = nil
            }
    }
}
#endif
