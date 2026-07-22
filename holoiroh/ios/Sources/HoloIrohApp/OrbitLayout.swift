import SwiftUI

/// Pure 3D-orbit placement math for the app badges circling the orb, split out
/// of `OrbEffects` (which is UIKit-bound via `AppBadge`) so the geometry is
/// deterministic and headlessly renderable: given a badge's index, the ring
/// phase, and the ring dimensions, it returns where / how big / how visible
/// that badge is.
///
/// It is a HORIZONTAL ring seen slightly from the front -- badges sweep from the
/// BACK of the orb (small, dim, high on screen, occluded by the blob) around to
/// the FRONT (large, bright, low on screen, drawn over everything), i.e. a real
/// 3D orbit rather than a flat top/bottom 2D ellipse.
struct OrbitBadgePlacement: Equatable {
    /// Screen offset from the orb center.
    var offset: CGSize
    /// Size multiplier (front badges larger).
    var scale: CGFloat
    /// 0...1 visibility (back-center badges fade behind the orb).
    var opacity: Double
    /// Gaussian softening for far/back badges.
    var blur: CGFloat
    /// Painter's-order key: higher = nearer the viewer, drawn on top.
    var z: Double
    /// Signed depth, 1 = dead front, -1 = dead back (handy for witnesses).
    var depth: Double
}

/// Place badge `index` of `count` on the ring at `phase` radians.
/// - `radiusX`: horizontal half-width of the ring (its wide axis).
/// - `tiltY`: vertical half-height (small -> a shallow, 3D-looking ring).
/// - `blobRadius`: the orb's on-screen radius, for back-of-ring occlusion.
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
