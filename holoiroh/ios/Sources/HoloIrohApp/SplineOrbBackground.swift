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
                let side = min(geo.size.width * 0.85, 420)
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
/// The native-runtime orb. `SplineView(sceneFileURL:)` is throwing; a load
/// failure (offline, scene URL gone) falls through to the gradient
/// underlay -- never an error surface on a background element.
private struct NativeSplineOrb: View {
    private let sceneURL = URL(string: "https://prod.spline.design/EEXJWT2Sfje1M4iS/scene.splinecode")!

    var body: some View {
        if let view = try? SplineView(sceneFileURL: sceneURL) {
            view
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
