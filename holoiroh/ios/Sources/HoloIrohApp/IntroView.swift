import SwiftUI

/// First-launch brand intro: the Aro orb blooms out of a black void with
/// concentric energy rings and drifting light particles, the "Aro" wordmark
/// materializes letter by letter, and a tagline settles beneath it -- then it
/// hands off to the pairing screen (`ContentView` gates and reveals it).
///
/// ## Architecture: progress-driven, so it is deterministic and renderable
/// The visuals are a *pure function* of a single `progress: Double` in `0...1`
/// (`IntroContent`). The outer `IntroView` is the only thing that turns wall
/// time into `progress` (a `TimelineView(.animation)`). Splitting it this way
/// means every visual state is reproducible at any `t` with no timers or
/// randomness -- which is exactly how the headless `ImageRenderer` witness
/// renders frames at t = 0.2 / 0.5 / 0.8 / 1.0 off-device.
///
/// Considerate by default: a `Skip` affordance (and a tap anywhere) ends it
/// early, and `accessibilityReduceMotion` collapses the particle/ring motion
/// into a calm fade so the intro is compelling without being an obstacle.

// MARK: - Easing / segment helpers

/// Fraction of the window `[a, b]` completed at `t`, clamped to `0...1`.
private func seg(_ t: Double, _ a: Double, _ b: Double) -> Double {
    guard b > a else { return t >= b ? 1 : 0 }
    return min(max((t - a) / (b - a), 0), 1)
}

/// Cubic ease-out -- fast start, gentle settle.
private func easeOut(_ x: Double) -> Double { 1 - pow(1 - x, 3) }

/// Fractional part, for deterministic per-particle variance from its index.
private func frac(_ x: Double) -> Double { x - floor(x) }

// MARK: - Pure visual content

/// The intro rendered at a fixed `progress` -- no state, no timers, no random.
struct IntroContent: View {
    /// 0 = pure void, 1 = fully assembled resting state.
    var progress: Double
    /// Calm variant: no particles, no expanding rings, gentle fades only.
    var reduceMotion: Bool = false

    var body: some View {
        GeometryReader { geo in
            let size = geo.size
            let minSide = min(size.width, size.height)
            // The whole composition eases upward a touch at the very end so the
            // orb comes to rest where the pairing header's glow sits -- the
            // reveal into pairing then reads as one continuous motion.
            let settle = easeOut(seg(progress, 0.82, 1.0))
            let cx = size.width / 2
            let cy = size.height * 0.40 - settle * (size.height * 0.03)

            ZStack {
                Color.black

                orbGlow(minSide: minSide).position(x: cx, y: cy)
                orbCore(minSide: minSide).position(x: cx, y: cy)

                if !reduceMotion {
                    bloomFlash(minSide: minSide).position(x: cx, y: cy)
                    Canvas { ctx, _ in
                        drawRings(ctx, center: CGPoint(x: cx, y: cy), minSide: minSide)
                        drawParticles(ctx, center: CGPoint(x: cx, y: cy), minSide: minSide)
                    }
                    .allowsHitTesting(false)
                } else {
                    Canvas { ctx, _ in
                        drawRings(ctx, center: CGPoint(x: cx, y: cy), minSide: minSide)
                    }
                    .allowsHitTesting(false)
                }

                VStack(spacing: 12) {
                    wordmark(minSide: minSide)
                    tagline
                }
                .position(x: cx, y: size.height * 0.66 - settle * (size.height * 0.015))
            }
            .opacity(easeOut(seg(progress, 0.0, 0.06)))   // fade up from full black
        }
        .ignoresSafeArea()
    }

    // Orb --------------------------------------------------------------------

    private func orbCore(minSide: CGFloat) -> some View {
        let t = easeOut(seg(progress, 0.0, 0.38))
        let d = minSide * 0.40 * t
        return Circle()
            .fill(
                RadialGradient(
                    colors: [.white, .aroAccentBright, .aroAccent, .aroAccent.opacity(0)],
                    center: UnitPoint(x: 0.42, y: 0.40),
                    startRadius: 0,
                    endRadius: max(d * 0.62, 1)
                )
            )
            .frame(width: d, height: d)
            .blur(radius: reduceMotion ? 0 : 1.5)
    }

    private func orbGlow(minSide: CGFloat) -> some View {
        let t = easeOut(seg(progress, 0.0, 0.5))
        let d = minSide * 0.40 * t
        return Circle()
            .fill(
                RadialGradient(
                    colors: [Color.aroAccent.opacity(0.55 * t), .clear],
                    center: .center, startRadius: 0, endRadius: max(d * 1.2, 1)
                )
            )
            .frame(width: max(d * 2.6, 1), height: max(d * 2.6, 1))
            .blur(radius: 26)
    }

    private func bloomFlash(minSide: CGFloat) -> some View {
        let f = seg(progress, 0.03, 0.5)
        let r = minSide * 0.10 + easeOut(f) * minSide * 0.5
        return Circle()
            .stroke(Color.white.opacity((1 - f) * 0.8), lineWidth: (1 - f) * 2.5 + 0.4)
            .frame(width: r * 2, height: r * 2)
            .opacity(f > 0 && f < 1 ? 1 : 0)
    }

    // Canvas: rings + particles ---------------------------------------------

    private func drawRings(_ ctx: GraphicsContext, center c: CGPoint, minSide: CGFloat) {
        for i in 0..<3 {
            let a = 0.16 + Double(i) * 0.11
            let rp = seg(progress, a, a + 0.5)
            guard rp > 0, rp < 1 else { continue }
            let radius = minSide * (0.16 + CGFloat(easeOut(rp)) * 0.5)
            let op = (1 - rp) * 0.5
            let rect = CGRect(x: c.x - radius, y: c.y - radius, width: radius * 2, height: radius * 2)
            ctx.stroke(Path(ellipseIn: rect), with: .color(Color.aroAccentBright.opacity(op)), lineWidth: 1.4)
        }
    }

    private func drawParticles(_ ctx: GraphicsContext, center c: CGPoint, minSide: CGFloat) {
        let count = 34
        let golden = 2.399963229728653   // golden angle (radians)
        for i in 0..<count {
            let f = Double(i)
            let angle = f * golden
            let speedVar = 0.65 + 0.35 * frac(f * 0.61803)
            let pStart = 0.10 + 0.22 * frac(f * 0.317)
            let pp = seg(progress, pStart, pStart + 0.55)
            guard pp > 0 else { continue }
            let dist = CGFloat(easeOut(pp)) * minSide * 0.55 * CGFloat(speedVar)
            let op = (1 - pp) * (1 - pp) * 0.85
            let px = c.x + CGFloat(cos(angle)) * dist
            let py = c.y + CGFloat(sin(angle)) * dist
            let r = CGFloat(1.4 + 2.2 * frac(f * 0.123))
            let rect = CGRect(x: px - r, y: py - r, width: r * 2, height: r * 2)
            ctx.fill(Path(ellipseIn: rect), with: .color(Color.aroAccentBright.opacity(op)))
        }
    }

    // Text -------------------------------------------------------------------

    private func wordmark(minSide: CGFloat) -> some View {
        let letterSize = min(minSide * 0.19, 72)
        return HStack(spacing: 1) {
            ForEach(Array("Aro".enumerated()), id: \.offset) { idx, ch in
                let base = 0.42 + Double(idx) * 0.07
                let lp = easeOut(seg(progress, base, base + 0.24))
                Text(String(ch))
                    .font(.system(size: letterSize, weight: .bold, design: .rounded))
                    .foregroundStyle(
                        LinearGradient(colors: [.white, .aroAccent], startPoint: .top, endPoint: .bottom)
                    )
                    .opacity(lp)
                    .offset(y: (1 - lp) * 20)
                    .blur(radius: reduceMotion ? 0 : (1 - lp) * 12)
            }
        }
        .accessibilityElement()
        .accessibilityLabel("Aro")
        .accessibilityAddTraits(.isHeader)
    }

    private var tagline: some View {
        let tp = easeOut(seg(progress, 0.64, 0.92))
        return Text("Your Mac, anywhere.")
            .font(.system(size: 15, weight: .medium, design: .rounded))
            .tracking(2.5)
            .foregroundStyle(.white.opacity(0.72 * tp))
            .opacity(tp)
            .offset(y: (1 - tp) * 12)
    }
}

// MARK: - Driver

/// Turns wall time into `progress` and drives `IntroContent`, then calls
/// `onFinished`. A tap anywhere or the Skip button ends it early.
struct IntroView: View {
    var onFinished: () -> Void

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var startDate: Date?
    @State private var finished = false
    @State private var showSkip = false

    /// Reduce-motion gets a shorter, calmer run.
    private var duration: Double { reduceMotion ? 1.7 : 3.3 }

    var body: some View {
        ZStack {
            if let startDate {
                TimelineView(.animation) { context in
                    let elapsed = context.date.timeIntervalSince(startDate)
                    let p = min(max(elapsed / duration, 0), 1)
                    IntroContent(progress: p, reduceMotion: reduceMotion)
                }
            } else {
                IntroContent(progress: 0, reduceMotion: reduceMotion)
            }
        }
        .ignoresSafeArea()
        .contentShape(Rectangle())
        .onTapGesture { finish() }
        .overlay(alignment: .bottom) {
            if showSkip {
                Button(action: finish) {
                    Text("Skip")
                        .font(.subheadline.weight(.medium))
                        .foregroundStyle(.white.opacity(0.45))
                        .padding(.horizontal, 20)
                        .padding(.vertical, 8)
                }
                .buttonStyle(.plain)
                .padding(.bottom, 36)
                .transition(.opacity)
                .accessibilityLabel("Skip intro")
            }
        }
        .onAppear {
            startDate = Date()
            DispatchQueue.main.asyncAfter(deadline: .now() + duration + 0.2) { finish() }
            withAnimation(.easeIn(duration: 0.4).delay(0.7)) { showSkip = true }
        }
    }

    private func finish() {
        guard !finished else { return }
        finished = true
        onFinished()
    }
}

#Preview("Intro t=0.5") {
    IntroContent(progress: 0.5)
}

#Preview("Intro driver") {
    IntroView(onFinished: {})
}
