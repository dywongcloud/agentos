import SwiftUI

/// Contains the rendered placement of one badge in a horizontal three-dimensional orbit.
/// Front badges appear lower, larger, sharper, and above back badges.
/// Back badges fade behind the orb near its center.
struct OrbitBadgePlacement: Equatable {
    /// Specifies the offset from the orb center.
    var offset: CGSize
    /// Specifies the size multiplier.
    var scale: CGFloat
    /// Specifies visibility from `0` through `1`.
    var opacity: Double
    /// Specifies Gaussian blur for back badges.
    var blur: CGFloat
    /// Specifies drawing order.
    /// Higher values are nearer the viewer.
    var z: Double
    /// Specifies signed depth from `-1` at the back to `1` at the front.
    var depth: Double
}

/// Calculates one badge placement on the orbit.
/// Values of `count` below `1` use `1`.
/// - Parameters:
///   - radiusX: The horizontal ring radius.
///   - tiltY: The vertical ring radius.
///   - blobRadius: The on-screen orb radius used for occlusion.
func orbitBadgePlacement(
    index: Int,
    count: Int,
    phase: Double,
    radiusX: CGFloat,
    tiltY: CGFloat,
    blobRadius: CGFloat
) -> OrbitBadgePlacement {
    let n = max(count, 1)
    let base = phase + (Double(index) / Double(n)) * 2 * .pi
    let horiz = sin(base)         // -1 (left) .. 1 (right)
    let dep = cos(base)           // 1 (front) .. -1 (back)
    let depth01 = (dep + 1) / 2   // 0 back .. 1 front

    let x = CGFloat(horiz) * radiusX
    // Front (+dep) sits lower on screen (nearer, over the orb's lower face);
    // back (-dep) sits higher (behind the orb's upper edge) -- the ring tilt.
    let y = CGFloat(dep) * tiltY

    // Depth cues: back badges are smaller and softer, front badges large + sharp.
    let scale = 0.58 + 0.62 * CGFloat(depth01)
    let blur = CGFloat(1 - depth01) * 3.0

    // Occlusion: a badge that is BOTH behind (dep < 0) and horizontally over the
    // orb's silhouette fades toward invisible -- it has passed behind the blob
    // and re-emerges on the far side.
    let backness = max(0.0, -dep)                                  // 0..1
    let centrality = blobRadius > 0 ? max(0.0, 1 - Double(abs(x) / blobRadius)) : 0
    let occlusion = 1 - backness * centrality                     // -> 0 at back-center
    let opacity = (0.55 + 0.45 * depth01) * occlusion

    return OrbitBadgePlacement(
        offset: CGSize(width: x, height: y),
        scale: scale,
        opacity: opacity,
        blur: blur,
        z: depth01,
        depth: dep
    )
}
