import CoreMedia
import CoreVideo

/// A producer of decoded video frames for `VideoRenderView` to display.
///
/// This protocol is the seam between the *render* half of the live-view
/// feature (owned by this file group -- `VideoRenderView` +
/// `SyntheticVideoFrameSource`) and the *source* half (the real
/// `iroh-live` subscription, which is not wired up yet -- see
/// `holoiroh/README.md` and `ios/IROH_FFI.md`). The view binds to a
/// `VideoFrameSource` and never constructs frames itself, so the real
/// network source can be dropped in later by writing one more type that
/// conforms to this protocol -- no change to `VideoRenderView` or
/// `MainView`.
///
/// ## What a conforming source delivers
/// A source pushes already-**decoded** frames to `onFrame`. It does *not*
/// decode compressed H.264/HEVC bitstream -- that is the source's own
/// internal concern before it calls `onFrame`. Concretely, the eventual
/// `iroh-live` source will:
/// 1. pull compressed access units from the `ios-bridge`
///    `holoiroh_ios_bridge_poll_next_frame` FFI (a `HoloirohFrame` with
///    `width`/`height`/`timestamp_us`/`kind`, see
///    `holoiroh/ios-bridge/src/lib.rs`),
/// 2. feed them through a `VTDecompressionSession` to produce a
///    `CVPixelBuffer` (or assemble a `CMSampleBuffer` directly from the
///    encoded NAL units + a `CMVideoFormatDescription`),
/// 3. hand that off via `onFrame(.pixelBuffer(...))` /
///    `onFrame(.sampleBuffer(...))`.
/// The synthetic source in this build skips step 1-2 and draws frames on
/// device instead, but hits the *same* `onFrame` seam -- which is exactly
/// what makes it a real witness of the render path.
///
/// ## Threading
/// `onFrame` may be invoked on any thread (a real network/decode source
/// will not be on the main thread). `VideoRenderView` guarantees its
/// `enqueue(...)` entry points are safe to call from any thread, so a
/// conforming source does not need to hop to the main thread before
/// delivering a frame.
protocol VideoFrameSource: AnyObject {
    /// Invoked once per decoded frame. Assign before `start()`. Set to
    /// `nil` (or replace) to redirect delivery; a source must tolerate
    /// `onFrame == nil` (drop the frame) rather than force-unwrapping.
    var onFrame: ((VideoFrame) -> Void)? { get set }

    /// Begin producing frames. Idempotent: calling `start()` on an
    /// already-started source is a no-op, not a double-start.
    func start()

    /// Stop producing frames and release any per-run resources (timers,
    /// decode sessions). Idempotent and safe to call from teardown.
    func stop()

    /// When the most recent frame was delivered to `onFrame`, or `nil` if
    /// none have been yet. Read from the main thread by `MainView`'s
    /// foreground-recovery liveness check: after an app switch, "restarted
    /// but no frame arrived within a few seconds" is the signal that the
    /// underlying transport died in the background and a frame-source
    /// restart alone cannot heal it (a full reconnect can). Implementations
    /// must make this safe to read from any thread.
    var lastFrameAt: Date? { get }
}

/// One decoded frame handed from a `VideoFrameSource` to `VideoRenderView`.
///
/// Two cases because there are two natural shapes a decode path lands in,
/// and the render view accepts both without the source having to convert:
/// - `.pixelBuffer`: a raw decoded image (what a `VTDecompressionSession`
///   output callback yields, and what the synthetic source draws). The
///   view wraps it in a `CMSampleBuffer` with display-immediately timing.
/// - `.sampleBuffer`: an already-assembled `CMSampleBuffer` (what a
///   decoder that emits sample buffers, or a demuxer, yields). The view
///   enqueues it directly, honoring whatever timing it carries.
enum VideoFrame {
    /// A decoded image plus the presentation time to show it at. `pts`
    /// (presentation timestamp) drives the display layer's scheduling;
    /// pass `.invalid` to mean "show immediately" (the view then stamps a
    /// display-immediately attachment).
    case pixelBuffer(CVPixelBuffer, pts: CMTime)

    /// A fully-formed sample buffer, enqueued as-is.
    case sampleBuffer(CMSampleBuffer)
}
