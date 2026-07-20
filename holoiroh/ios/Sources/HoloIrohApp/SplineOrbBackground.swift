import SwiftUI
#if canImport(SplineRuntime)
import SplineRuntime
#endif
#if canImport(WebKit)
import WebKit
#endif

/// Full-screen animated background: the blue blob orb Spline scene
/// (`prod.spline.design/EEXJWT2Sfje1M4iS`).
///
/// Rendering backends, best-first:
/// 1. **Native SplineRuntime** (Metal; the official splinetool iOS SPM
///    package) loading the exact public `scene.splinecode` URL -- matches
///    the reference render (deep black field, glowing blue blob) that the
///    earlier web-runtime approach visibly degraded on device.
/// 2. **Web runtime in a WKWebView** where SplineRuntime can't be imported
///    (kept as the middle fallback so non-SPM build contexts still show
///    the real scene).
/// 3. **Deep blue-black gradient** while either loads, or offline.
///
/// Non-interactive: all touches pass through to the app's real controls
/// layered above it.
struct SplineOrbBackground: View {
    var body: some View {
        ZStack(alignment: .top) {
            // Underlay: near-black field matching the reference render's
            // background, shown while the scene loads (or offline).
            Color.black

            // The orb scene is framed in a top-centered SQUARE canvas, not
            // full-bleed: the Spline scene's camera is composed for a
            // roughly square/landscape viewport, and stretching it across a
            // tall phone screen pushed the blob itself out of frame --
            // live-witnessed as "all I see is a blue glow but not the
            // actual orb". A square canvas ~85% of screen width, pinned
            // near the top, frames the blob exactly like the reference
            // render (orb top-center, black field everywhere else).
            GeometryReader { geo in
                // 1.25x on top of the prior 1.2x zoom (user-requested "25%
                // larger" on top of the current size -- base 0.85-width
                // square x 1.2 x 1.25 = x1.275, cap 420 x 1.2 x 1.25 = 630):
                // the scene renders natively at the bigger size, so the orb
                // grows without post-render scaling blur.
                let side = min(geo.size.width * 1.275, 630)
                Group {
                    #if canImport(SplineRuntime)
                    NativeSplineOrb()
                    #elseif canImport(WebKit)
                    SplineWebView()
                    #endif
                }
                .frame(width: side, height: side)
                .position(x: geo.size.width / 2, y: side / 2 + 24)
                .allowsHitTesting(false)
            }
        }
        .ignoresSafeArea()
    }
}

#if canImport(SplineRuntime)
/// The native-runtime orb, using the REAL SplineView API: the init is
/// NON-throwing (`init(sceneFileURL: URL?, ...)` -- the old
/// `try SplineView(...)` tutorial pattern is a no-op on 0.2.x), and every
/// load failure is delivered to the phase closure as
/// `.failure(SplineViewError)`. The DEFAULT closure renders failures as a
/// blank view with zero diagnostics -- exactly how "the orb doesn't
/// render, just black" stayed undiagnosable. This explicit closure logs
/// the concrete error case (fileUnknownFormat / fileOldFormat /
/// fileNewFormat / fileUnreachable / deviceUnsupported) to the device
/// console instead.
///
/// File preference order:
/// 1. Bundled `orb.splineswift` -- the iOS runtime's REAL input format
///    (only produced by the Spline editor's Export -> Mobile Platform ->
///    Apple panel; not yet exported for this scene).
/// 2. Bundled `orb.splinecode` -- the WEB runtime format; the iOS runtime
///    accepts the URL but fails format validation (research-verified:
///    docs + binary-strings audit of SplineRuntime 0.2.53). Kept only as
///    a candidate in case a future runtime accepts it.
/// 3. Remote scene.splinecode URL, same caveat.
private struct NativeSplineOrb: View {
    /// The user's real Apple-platform export of this scene (Export ->
    /// Mobile Platform -> Apple), NOT the web `.splinecode` URL.
    private static let remoteURL = URL(string: "https://build.spline.design/FxpXnkTaazJVlLqe096P/scene.splineswift")!

    private var sceneURL: URL {
        Bundle.module.url(forResource: "orb", withExtension: "splineswift")
            ?? Self.remoteURL
    }

    var body: some View {
        let url = sceneURL
        SplineView(sceneFileURL: url) { phase in
            switch phase {
            case .success(let content):
                content
            case .empty:
                Color.clear
            case .failure(let error):
                // LOUD in the console, invisible on screen (the gradient
                // underlay carries the design while broken).
                Color.clear.onAppear {
                    NSLog("SplineOrbBackground: SplineView FAILED for \(url.lastPathComponent): \(error) -- if fileUnknownFormat/fileNewFormat, the scene needs a .splineswift export from the Spline editor (Export -> Mobile Platform -> Apple)")
                }
            }
        }
    }
}
#endif

#if canImport(WebKit)
private struct SplineWebView: UIViewRepresentable {
    func makeUIView(context: Context) -> WKWebView {
        let config = WKWebViewConfiguration()
        config.allowsInlineMediaPlayback = true
        let webView = WKWebView(frame: .zero, configuration: config)
        webView.isOpaque = false
        webView.backgroundColor = .clear
        webView.scrollView.isScrollEnabled = false
        webView.isUserInteractionEnabled = false

        // Inline page: canvas fills the viewport, Spline web runtime loads
        // the exact public scene URL. A module import from unpkg needs a
        // real (non-file) base URL for CORS-clean fetches.
        let html = """
        <!DOCTYPE html>
        <html><head>
        <meta name="viewport" content="width=device-width, initial-scale=1, maximum-scale=1">
        <style>
          html, body { margin: 0; padding: 0; background: transparent; overflow: hidden; }
          canvas { width: 100vw; height: 100vh; display: block; }
        </style>
        </head><body>
        <canvas id="orb"></canvas>
        <script type="module">
          import { Application } from 'https://unpkg.com/@splinetool/runtime';
          const app = new Application(document.getElementById('orb'));
          app.load('https://prod.spline.design/EEXJWT2Sfje1M4iS/scene.splinecode');
        </script>
        </body></html>
        """
        webView.loadHTMLString(html, baseURL: URL(string: "https://prod.spline.design"))
        return webView
    }

    func updateUIView(_ uiView: WKWebView, context: Context) {}
}
#endif
