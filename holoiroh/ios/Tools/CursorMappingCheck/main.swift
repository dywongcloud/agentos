import CoreGraphics
import Foundation

// Witnesses the touch -> desktop mapping used while the user drives the Mac by hand, at every
// zoom level the pinch gesture can reach. Compiles the REAL `VideoViewportTransform` and the
// REAL `normalizedInVideo` -- no view, no device, no running daemon.

var failures = 0

func check(_ condition: Bool, _ what: String) {
    if condition {
        print("  ok   \(what)")
    } else {
        print("  FAIL \(what)")
        failures += 1
    }
}

func closeEnough(_ a: CGPoint, _ b: CGPoint, tolerance: CGFloat = 0.0001) -> Bool {
    abs(a.x - b.x) < tolerance && abs(a.y - b.y) < tolerance
}

/// Where a normalized video point is drawn inside the viewport BEFORE the pinch/pan transform:
/// the aspect-fit inverse of `normalizedInVideo`.
func aspectFitPoint(normalized n: CGPoint, viewSize: CGSize, frameSize: CGSize) -> CGPoint {
    let viewAspect = viewSize.width / viewSize.height
    let frameAspect = frameSize.width / frameSize.height
    var vw = viewSize.width
    var vh = viewSize.height
    var ox: CGFloat = 0
    var oy: CGFloat = 0
    if frameAspect > viewAspect {
        vh = viewSize.width / frameAspect
        oy = (viewSize.height - vh) / 2
    } else {
        vw = viewSize.height * frameAspect
        ox = (viewSize.width - vw) / 2
    }
    return CGPoint(x: ox + n.x * vw, y: oy + n.y * vh)
}

/// Where that point actually appears on glass once the renderer's `.scaleEffect` + `.offset`
/// are applied -- the forward direction of what `RemoteControlSurface` has to invert.
func onScreen(_ contentPoint: CGPoint, transform: VideoViewportTransform, viewport: CGSize) -> CGPoint {
    CGPoint(
        x: viewport.width / 2 + (contentPoint.x - viewport.width / 2) * transform.scale + transform.offset.width,
        y: viewport.height / 2 + (contentPoint.y - viewport.height / 2) * transform.scale + transform.offset.height
    )
}

/// The production mapping, exactly as `RemoteControlSurface.Coordinator.normalized` performs it.
func touchToDesktop(
    _ touch: CGPoint,
    zoom: CGFloat,
    pan: CGSize,
    viewport: CGSize,
    frameSize: CGSize
) -> CGPoint? {
    let transform = VideoViewportTransform(zoom: zoom, pan: pan, viewport: viewport)
    let inContent = transform.viewportPointToContent(touch, viewport: viewport)
    return normalizedInVideo(touch: inContent, viewSize: viewport, frameSize: frameSize)
}

/// The mapping as it behaved BEFORE the transform was accounted for, kept here solely so the
/// regression assertions below can prove the difference is large rather than cosmetic.
func touchToDesktopIgnoringZoom(
    _ touch: CGPoint,
    viewport: CGSize,
    frameSize: CGSize
) -> CGPoint? {
    normalizedInVideo(touch: touch, viewSize: viewport, frameSize: frameSize)
}

let viewport = CGSize(width: 390, height: 220)
let frameSize = CGSize(width: 3024, height: 1964)
let zoomLevels: [CGFloat] = [1, 1.5, 2, 3, 5]
let pans: [CGSize] = [.zero, CGSize(width: 40, height: -25), CGSize(width: -120, height: 60)]

print("aiming lands where the user is looking, at every zoom level")
for zoom in zoomLevels {
    for pan in pans {
        let transform = VideoViewportTransform(zoom: zoom, pan: pan, viewport: viewport)
        for target in [
            CGPoint(x: 0.5, y: 0.5),
            CGPoint(x: 0.12, y: 0.34),
            CGPoint(x: 0.78, y: 0.66),
        ] {
            let content = aspectFitPoint(normalized: target, viewSize: viewport, frameSize: frameSize)
            let glass = onScreen(content, transform: transform, viewport: viewport)
            guard let landed = touchToDesktop(glass, zoom: zoom, pan: pan, viewport: viewport, frameSize: frameSize) else {
                check(false, "zoom \(zoom) pan \(pan.width),\(pan.height): touching \(target) mapped to nothing")
                continue
            }
            check(
                closeEnough(landed, target),
                "zoom \(zoom) pan \(pan.width),\(pan.height): touching the pixel showing \(target) drives the cursor there"
            )
        }
    }
}

print("\nunzoomed behaviour is untouched")
for touch in [CGPoint(x: 10, y: 10), CGPoint(x: 195, y: 110), CGPoint(x: 380, y: 210)] {
    let now = touchToDesktop(touch, zoom: 1, pan: .zero, viewport: viewport, frameSize: frameSize)
    let before = touchToDesktopIgnoringZoom(touch, viewport: viewport, frameSize: frameSize)
    switch (now, before) {
    case let (n?, b?):
        check(closeEnough(n, b), "touch \(touch) maps identically at zoom 1")
    case (nil, nil):
        check(true, "touch \(touch) is letterbox in both")
    default:
        check(false, "touch \(touch) disagrees about being on the video at zoom 1")
    }
}

print("\nthe zoomed mis-aim this guards against is large, not cosmetic")
let offCenter = CGPoint(x: 320, y: 60)
for zoom in zoomLevels where zoom > 1 {
    guard let correct = touchToDesktop(offCenter, zoom: zoom, pan: .zero, viewport: viewport, frameSize: frameSize),
          let ignoring = touchToDesktopIgnoringZoom(offCenter, viewport: viewport, frameSize: frameSize) else {
        check(false, "zoom \(zoom): expected both mappings to produce a point")
        continue
    }
    let driftX = abs(correct.x - ignoring.x) * frameSize.width
    check(
        driftX > 100,
        "zoom \(zoom): ignoring the transform would land \(Int(driftX)) desktop px away horizontally"
    )
}

print("\nzooming buys precision: the same finger travel moves the cursor less")
let travel: CGFloat = 20
for zoom in zoomLevels {
    let a = CGPoint(x: 190, y: 110)
    let b = CGPoint(x: 190 + travel, y: 110)
    guard let na = touchToDesktop(a, zoom: zoom, pan: .zero, viewport: viewport, frameSize: frameSize),
          let nb = touchToDesktop(b, zoom: zoom, pan: .zero, viewport: viewport, frameSize: frameSize) else {
        check(false, "zoom \(zoom): center drag mapped to nothing")
        continue
    }
    let movedPx = (nb.x - na.x) * frameSize.width
    let expected = (travel / (viewport.height * frameSize.width / frameSize.height)) * frameSize.width / zoom
    check(
        abs(movedPx - expected) < 1.0,
        "zoom \(zoom): a \(Int(travel))pt drag moves the cursor \(Int(movedPx)) desktop px"
    )
}

print("\npanning can never reveal backdrop behind the video")
for zoom in zoomLevels {
    for pan in [CGSize(width: 9999, height: 9999), CGSize(width: -9999, height: -9999)] {
        let transform = VideoViewportTransform(zoom: zoom, pan: pan, viewport: viewport)
        let maxX = max(0, viewport.width * (transform.scale - 1) / 2)
        let maxY = max(0, viewport.height * (transform.scale - 1) / 2)
        check(
            abs(transform.offset.width) <= maxX + 0.0001 && abs(transform.offset.height) <= maxY + 0.0001,
            "zoom \(zoom): a runaway pan of \(Int(pan.width)) is clamped to \(Int(transform.offset.width))"
        )
    }
}

print("\nzoom stays inside the range the pinch gesture commits")
for (requested, expected) in [(CGFloat(0.1), CGFloat(1)), (CGFloat(1), CGFloat(1)), (CGFloat(500), CGFloat(5))] {
    let transform = VideoViewportTransform(zoom: requested, pan: .zero, viewport: viewport)
    check(transform.scale == expected, "a requested zoom of \(requested) resolves to \(transform.scale)")
}

print("\nletterbox touches are still rejected rather than sent as a wild coordinate")
let tallFrame = CGSize(width: 800, height: 2000)
check(
    touchToDesktop(CGPoint(x: 5, y: 110), zoom: 1, pan: .zero, viewport: viewport, frameSize: tallFrame) == nil,
    "a touch in the side bar of a tall frame maps to nothing"
)
check(
    touchToDesktop(CGPoint(x: 195, y: 110), zoom: 1, pan: .zero, viewport: viewport, frameSize: tallFrame) != nil,
    "a touch on the image itself still maps"
)

print("\na finger straying into the letterbox slides the cursor along the edge instead of freezing")
let wideFrame = CGSize(width: 3024, height: 1000)
for touch in [
    CGPoint(x: 195, y: -50),
    CGPoint(x: 195, y: 0),
    CGPoint(x: 195, y: 270),
    CGPoint(x: -30, y: 110),
    CGPoint(x: 900, y: 110),
] {
    guard let clamped = normalizedInVideoClampedToEdges(touch: touch, viewSize: viewport, frameSize: wideFrame) else {
        check(false, "touch \(touch) produced no clamped point")
        continue
    }
    check(
        clamped.x >= 0 && clamped.x <= 1 && clamped.y >= 0 && clamped.y <= 1,
        "touch \(touch) clamps to \(clamped) instead of dropping the drag"
    )
}
check(
    normalizedInVideo(touch: CGPoint(x: 195, y: 5), viewSize: viewport, frameSize: wideFrame) == nil,
    "the strict mapping still rejects a letterbox touch, so taps do not click the screen edge"
)

print("\nthe clamped and strict mappings never disagree about the image itself")
for frame in [frameSize, wideFrame, CGSize(width: 800, height: 2000)] {
    for touch in [CGPoint(x: 195, y: 110), CGPoint(x: 150, y: 95), CGPoint(x: 240, y: 130)] {
        guard let strict = normalizedInVideo(touch: touch, viewSize: viewport, frameSize: frame) else { continue }
        guard let clamped = normalizedInVideoClampedToEdges(touch: touch, viewSize: viewport, frameSize: frame) else {
            check(false, "frame \(frame): clamped mapping produced nothing where strict did")
            continue
        }
        check(closeEnough(strict, clamped), "frame \(frame): touch \(touch) maps identically either way")
    }
}

print("\ndegenerate sizes still map to nothing rather than to a wild coordinate")
check(
    normalizedInVideoClampedToEdges(touch: CGPoint(x: 10, y: 10), viewSize: .zero, frameSize: frameSize) == nil,
    "a zero-sized view maps to nothing"
)
check(
    normalizedInVideoClampedToEdges(touch: CGPoint(x: 10, y: 10), viewSize: viewport, frameSize: .zero) == nil,
    "a zero-sized frame maps to nothing"
)

// Everything above exercises the math directly. None of it compiles the PRODUCTION call site:
// `RemoteControlSurface.Coordinator` lives behind `#if canImport(UIKit)`, so on macOS it is not
// built at all, and a revert that simply stopped applying the transform there would leave every
// assertion above still passing. These guards read the real source so that revert is caught.
print("\nthe app actually routes touches through the transform")
let sourceDir = ProcessInfo.processInfo.environment["HOLOIROH_SWIFT_SOURCES"] ?? ""
check(!sourceDir.isEmpty, "run.sh exported the source directory to scan")

func source(_ name: String) -> String {
    (try? String(contentsOfFile: "\(sourceDir)/\(name)", encoding: .utf8)) ?? ""
}

let remoteControlSource = source("RemoteControl.swift")
let mainViewSource = source("MainView.swift")
let panZoomSource = source("PanZoomVideoSurface.swift")

check(!remoteControlSource.isEmpty, "RemoteControl.swift is readable")
check(
    remoteControlSource.contains("VideoViewportTransform(zoom: parent.zoom, pan: parent.pan"),
    "Coordinator.normalized builds the live transform from the surface's zoom/pan"
)
check(
    remoteControlSource.contains("viewportPointToContent"),
    "Coordinator.normalized undoes the transform before normalizing"
)
check(
    mainViewSource.contains("zoom: zoomScale") && mainViewSource.contains("pan: panOffset"),
    "MainView hands the live zoom/pan to RemoteControlSurface"
)
check(
    panZoomSource.contains("VideoViewportTransform("),
    "the renderer derives its scale/offset from the same type the touch mapping inverts"
)
check(
    !panZoomSource.contains("none of those depend on the live gesture scale/offset"),
    "the stale doc claiming the overlays ignore the transform is gone"
)
check(
    remoteControlSource.contains("guard let n = normalizedClamped(loc, in: v)"),
    "the one-finger drag maps through the edge-clamping variant"
)
check(
    remoteControlSource.contains("let n = normalizedClamped(g.location(in: v), in: v)"),
    "the two-finger scroll maps through the edge-clamping variant"
)
check(
    remoteControlSource.contains("guard let v = g.view, let n = normalized(g.location(in: v), in: v) else { return }\n            parent.onEvent(.click"),
    "a tap still uses the strict mapping, so tapping a black bar does not click the screen edge"
)

if failures == 0 {
    print("\nVERDICT: OK -- touch mapping is correct at every zoom level, and zoom now sharpens aim instead of breaking it")
    exit(0)
}
print("\nVERDICT: \(failures) FAILURE(S)")
exit(1)
