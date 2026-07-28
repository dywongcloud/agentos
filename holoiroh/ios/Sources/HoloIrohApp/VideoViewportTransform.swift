import CoreGraphics

/// The single definition of how the live-share video is scaled and panned inside its viewport.
///
/// Both the renderer and the remote-control touch mapping resolve through this one type on
/// purpose: they are inverses of each other, and when they were two separate copies the touch
/// side simply omitted the transform, so zooming in to aim moved the Mac cursor somewhere else.
struct VideoViewportTransform {
    static let zoomRange: ClosedRange<CGFloat> = 1...5

    let scale: CGFloat
    let offset: CGSize

    init(zoom: CGFloat, pan: CGSize, viewport: CGSize) {
        let clamped = Self.clampZoom(zoom)
        scale = clamped
        offset = Self.clampedPan(pan, scale: clamped, viewport: viewport)
    }

    static func clampZoom(_ value: CGFloat) -> CGFloat {
        min(max(value, zoomRange.lowerBound), zoomRange.upperBound)
    }

    /// Keeps the scaled content covering the whole viewport: with center-anchored scaling the
    /// content edge reaches the viewport edge at +/- viewport * (scale - 1) / 2.
    static func clampedPan(_ proposed: CGSize, scale: CGFloat, viewport: CGSize) -> CGSize {
        let maxX = max(0, viewport.width * (scale - 1) / 2)
        let maxY = max(0, viewport.height * (scale - 1) / 2)
        return CGSize(
            width: min(max(proposed.width, -maxX), maxX),
            height: min(max(proposed.height, -maxY), maxY)
        )
    }

    /// Where a touch at `point` in viewport coordinates lands in the video's own untransformed
    /// layout space -- the inverse of the `.scaleEffect` + `.offset` the renderer applies.
    /// Zooming in shrinks the mapped delta by `scale`, which is what makes a zoomed view give
    /// genuinely finer cursor control instead of merely a bigger picture.
    func viewportPointToContent(_ point: CGPoint, viewport: CGSize) -> CGPoint {
        let centerX = viewport.width / 2
        let centerY = viewport.height / 2
        return CGPoint(
            x: centerX + (point.x - centerX - offset.width) / scale,
            y: centerY + (point.y - centerY - offset.height) / scale
        )
    }
}
