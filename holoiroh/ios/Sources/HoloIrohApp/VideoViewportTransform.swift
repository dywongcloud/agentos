import CoreGraphics

/// Defines the shared scale and pan transform for video rendering and touch mapping.
/// Rendering and touch mapping use this type as inverse operations.
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

    /// Clamps pan so scaled content covers the viewport.
    /// Center-anchored scaling permits `viewport * (scale - 1) / 2` movement per axis.
    static func clampedPan(_ proposed: CGSize, scale: CGFloat, viewport: CGSize) -> CGSize {
        let maxX = max(0, viewport.width * (scale - 1) / 2)
        let maxY = max(0, viewport.height * (scale - 1) / 2)
        return CGSize(
            width: min(max(proposed.width, -maxX), maxX),
            height: min(max(proposed.height, -maxY), maxY)
        )
    }

    /// Maps a viewport point into untransformed video layout coordinates.
    /// This operation reverses the renderer's scale and offset.
    /// A larger scale reduces mapped movement from the viewport center.
    func viewportPointToContent(_ point: CGPoint, viewport: CGSize) -> CGPoint {
        let centerX = viewport.width / 2
        let centerY = viewport.height / 2
        return CGPoint(
            x: centerX + (point.x - centerX - offset.width) / scale,
            y: centerY + (point.y - centerY - offset.height) / scale
        )
    }
}
