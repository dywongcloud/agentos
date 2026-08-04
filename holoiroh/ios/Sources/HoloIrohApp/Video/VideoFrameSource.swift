import CoreMedia
import CoreVideo

/// Defines a source of decoded frames for `VideoRenderView`.
///
/// `IrohLiveFrameSource` implements the current media stream path.
/// `SyntheticVideoFrameSource` provides local diagnostic frames.
/// Conforming sources decode frames before delivery.
/// They can invoke `onFrame` from any thread.
protocol VideoFrameSource: AnyObject {
    /// Receives each decoded frame.
    /// Set this callback before `start()`.
    /// A source must drop frames safely when the callback is nil.
    var onFrame: ((VideoFrame) -> Void)? { get set }

    /// Starts frame production.
    /// Repeated calls while running have no additional effect.
    func start()

    /// Stops frame production and releases per-run resources.
    /// Repeated teardown calls must be safe.
    func stop()

    /// Reports the most recent frame delivery time.
    /// Returns `nil` before the first delivery.
    /// Implementations must support reads from any thread.
    var lastFrameAt: Date? { get }

    /// Reports the most recent frame dimensions.
    /// Returns `nil` before the first delivery.
    /// Main-thread reads must be safe.
    var lastFrameSize: CGSize? { get }
}

/// Contains one decoded frame in either supported render form.
///
/// - `pixelBuffer`: A decoded image with presentation timing.
/// - `sampleBuffer`: A complete sample buffer with its own timing.
enum VideoFrame {
    /// Contains a decoded image and its presentation timestamp.
    /// Use an invalid timestamp to request immediate presentation.
    case pixelBuffer(CVPixelBuffer, pts: CMTime)

    /// Contains a complete sample buffer for direct enqueue.
    case sampleBuffer(CMSampleBuffer)
}
