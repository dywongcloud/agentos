import SwiftUI
#if canImport(UIKit)
import UIKit
private typealias PlatformImage = UIImage
#elseif canImport(AppKit)
import AppKit
private typealias PlatformImage = NSImage
#endif

/// Renders the orb reaction after the app sends a message.
///
/// The overlay can show these effects:
/// - A pulse.
/// - A glow.
/// - Badges for apps that the prompt names.
///
/// The overlay uses these `SplineOrbBackground` layout values:
/// - A top-centered square canvas.
/// - `side = min(width * 1.275, 630)`.
/// - A center at `(width / 2, side / 2 + 24)`.
///
/// This layout supports the native runtime, web fallback, and offline fallback.
/// The overlay does not access the opaque Spline scene.
/// Hit testing is disabled so the overlay does not intercept control taps.

// MARK: - App catalog

/// Describes an app badge and its prompt-matching keywords.
/// Matching ignores case and requires word boundaries.
/// Bundled icons are 128-pixel Portable Network Graphics (PNG) files under `Resources/AppIcons`.
/// Other apps use a colored square and an SF Symbol.
struct OrbitApp: Identifiable, Equatable {
    let id: String
    let displayName: String
    let symbol: String
    let color: Color
    let keywords: [String]
    /// Names a bundled icon in `Resources/AppIcons` without the extension.
    var iconAsset: String? = nil

    /// Lists recognizable apps in match-priority order.
    static let catalog: [OrbitApp] = [
        OrbitApp(id: "slack", displayName: "Slack", symbol: "number",
                 color: Color(red: 0.29, green: 0.08, blue: 0.29),
                 keywords: ["slack"], iconAsset: "slack"),
        OrbitApp(id: "chrome", displayName: "Chrome", symbol: "globe",
                 color: Color(red: 0.26, green: 0.52, blue: 0.96),
                 keywords: ["chrome", "google chrome"], iconAsset: "chrome"),
        OrbitApp(id: "safari", displayName: "Safari", symbol: "safari",
                 color: Color(red: 0.0, green: 0.48, blue: 1.0),
                 keywords: ["safari", "browser"]),
        OrbitApp(id: "vscode", displayName: "VS Code", symbol: "chevron.left.forwardslash.chevron.right",
                 color: Color(red: 0.0, green: 0.48, blue: 0.80),
                 keywords: ["vscode", "vs code", "visual studio code"], iconAsset: "vscode"),
        OrbitApp(id: "docker", displayName: "Docker", symbol: "shippingbox",
                 color: Color(red: 0.11, green: 0.56, blue: 0.95),
                 keywords: ["docker", "container", "dockerfile"], iconAsset: "docker"),
        OrbitApp(id: "kubernetes", displayName: "Kubernetes", symbol: "helm",
                 color: Color(red: 0.20, green: 0.45, blue: 0.84),
                 keywords: ["kubernetes", "k8s", "kubectl"], iconAsset: "kubernetes"),
        OrbitApp(id: "claude", displayName: "Claude", symbol: "sparkles",
                 color: Color(red: 0.85, green: 0.45, blue: 0.25),
                 keywords: ["claude", "claude code"], iconAsset: "claude"),
        OrbitApp(id: "terminal", displayName: "Terminal", symbol: "terminal",
                 color: Color(red: 0.12, green: 0.12, blue: 0.14),
                 keywords: ["ghostty", "ghosty", "terminal", "shell", "zsh", "bash", "command line"]),
        OrbitApp(id: "xcode", displayName: "Xcode", symbol: "hammer",
                 color: Color(red: 0.11, green: 0.46, blue: 0.85),
                 keywords: ["xcode"]),
        OrbitApp(id: "mail", displayName: "Mail", symbol: "envelope",
                 color: Color(red: 0.10, green: 0.53, blue: 0.93),
                 keywords: ["mail", "email", "e-mail", "gmail", "inbox"]),
        OrbitApp(id: "messages", displayName: "Messages", symbol: "message",
                 color: Color(red: 0.20, green: 0.78, blue: 0.35),
                 keywords: ["imessage", "messages", "text message"]),
        OrbitApp(id: "calendar", displayName: "Calendar", symbol: "calendar",
                 color: Color(red: 0.92, green: 0.26, blue: 0.21),
                 keywords: ["calendar", "meeting invite"]),
        OrbitApp(id: "notes", displayName: "Notes", symbol: "note.text",
                 color: Color(red: 0.95, green: 0.77, blue: 0.06),
                 keywords: ["apple notes", "notes app", "notes"]),
        OrbitApp(id: "finder", displayName: "Finder", symbol: "folder",
                 color: Color(red: 0.25, green: 0.60, blue: 0.96),
                 keywords: ["finder"]),
        OrbitApp(id: "music", displayName: "Music", symbol: "music.note",
                 color: Color(red: 0.98, green: 0.24, blue: 0.44),
                 keywords: ["spotify", "apple music", "music"]),
    ]

    /// Returns catalog apps named in `prompt`.
    /// Results preserve catalog order and contain no duplicates.
    /// Matching ignores case and requires word boundaries.
    static func matches(in prompt: String) -> [OrbitApp] {
        let lowered = prompt.lowercased()
        return catalog.filter { app in
            app.keywords.contains { keyword in
                lowered.range(
                    of: "\\b" + NSRegularExpression.escapedPattern(for: keyword) + "\\b",
                    options: .regularExpression
                ) != nil
            }
        }
    }
}

// MARK: - Reaction state

/// Manages reaction state for sent prompts.
/// `react(to:)` restarts the effects and selects badges from prompt text.
/// The state clears after the configured duration.
@MainActor
final class OrbEffectsState: ObservableObject {
    /// Changes for each reaction so one-shot ring animations restart.
    @Published private(set) var reactionID = 0
    @Published private(set) var isReacting = false
    @Published private(set) var orbitingApps: [OrbitApp] = []

    private var clearTask: Task<Void, Never>?

    /// Starts or restarts the reaction for a sent prompt.
    func react(to prompt: String, duration: TimeInterval = 2.8) {
        reactionID += 1
        orbitingApps = OrbitApp.matches(in: prompt)
        isReacting = true
        // Optional high-tech cue (opt-in; best-effort; never blocks the visual).
        OrbSound.playReaction()
        let id = reactionID
        clearTask?.cancel()
        clearTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(duration * 1_000_000_000))
            guard let self, !Task.isCancelled, self.reactionID == id else { return }
            self.isReacting = false
        }
    }
}

// MARK: - Overlay

/// Renders pulse rings, a glow, and app badges above `SplineOrbBackground`.
struct OrbReactionOverlay: View {
    @ObservedObject var state: OrbEffectsState

    var body: some View {
        GeometryReader { geo in
            // EXACT mirror of SplineOrbBackground's canvas math, so the
            // effects stay centered on the blob across devices.
            let side = min(geo.size.width * 1.275, 630)
            let center = CGPoint(x: geo.size.width / 2, y: side / 2 + 24)
            // The blob itself fills roughly the middle ~55% of the canvas;
            // rings start just outside it and the badges orbit clear of it.
            let blobRadius = side * 0.26

            ZStack {
                if state.isReacting {
                    // Breathing glow: the orb's own palette, swelling gently
                    // for the reaction window.
                    BreathingGlow(radius: blobRadius * 1.35)
                        .position(center)
                        .transition(.opacity)

                    // One-shot expanding rings, restarted per reaction.
                    PulseRings(startRadius: blobRadius * 0.9)
                        .id(state.reactionID)
                        .position(center)
                        .transition(.opacity)
                }

                // Orbiting app badges (only when the prompt named apps): a 3D
                // horizontal ring that sweeps from behind the orb to the front.
                if !state.orbitingApps.isEmpty {
                    OrbitingBadges(
                        apps: state.orbitingApps,
                        radiusX: side * 0.40,
                        tiltY: side * 0.12,
                        blobRadius: blobRadius,
                        badgeSize: 44,
                        active: state.isReacting
                    )
                    .position(center)
                }
            }
            .animation(.easeInOut(duration: 0.45), value: state.isReacting)
        }
        .ignoresSafeArea()
        .allowsHitTesting(false)
    }
}

/// Renders a radial glow that changes scale and opacity during a reaction.
private struct BreathingGlow: View {
    let radius: CGFloat
    @State private var swelled = false

    var body: some View {
        Circle()
            .fill(
                RadialGradient(
                    colors: [
                        Color.aroAccent.opacity(0.34),
                        Color.aroAccent.opacity(0.10),
                        .clear,
                    ],
                    center: .center,
                    startRadius: radius * 0.2,
                    endRadius: radius
                )
            )
            .frame(width: radius * 2, height: radius * 2)
            .scaleEffect(swelled ? 1.16 : 0.94)
            .opacity(swelled ? 1.0 : 0.75)
            .onAppear {
                withAnimation(.easeInOut(duration: 0.9).repeatForever(autoreverses: true)) {
                    swelled = true
                }
            }
    }
}

/// Renders three staggered rings that expand once for each reaction.
private struct PulseRings: View {
    let startRadius: CGFloat

    var body: some View {
        ZStack {
            ForEach(0..<3, id: \.self) { index in
                PulseRing(startRadius: startRadius, delay: Double(index) * 0.42)
            }
        }
    }
}

private struct PulseRing: View {
    let startRadius: CGFloat
    let delay: Double
    @State private var expanded = false

    var body: some View {
        Circle()
            .stroke(
                Color.aroAccentBright.opacity(expanded ? 0.0 : 0.55),
                lineWidth: expanded ? 1 : 2.5
            )
            .frame(width: startRadius * 2, height: startRadius * 2)
            .scaleEffect(expanded ? 1.9 : 1.0)
            .onAppear {
                withAnimation(.easeOut(duration: 1.5).delay(delay)) {
                    expanded = true
                }
            }
    }
}

/// Animates app badges around a horizontal three-dimensional ring.
/// `TimelineView` pauses when the reaction is inactive.
/// `orbitBadgePlacement` provides deterministic geometry for each frame.
private struct OrbitingBadges: View {
    let apps: [OrbitApp]
    /// Sets the horizontal half-width of the ring.
    let radiusX: CGFloat
    /// Sets the vertical half-height of the ring.
    let tiltY: CGFloat
    /// Sets the orb radius used for rear-ring occlusion.
    let blobRadius: CGFloat
    /// Sets the badge edge length in points.
    let badgeSize: CGFloat
    let active: Bool

    /// Sets the angular speed in radians per second.
    private let angularSpeed = 1.0

    var body: some View {
        TimelineView(.animation(minimumInterval: nil, paused: !active)) { context in
            let phase = context.date.timeIntervalSinceReferenceDate * angularSpeed
            OrbitingBadgesRing(
                apps: apps,
                radiusX: radiusX,
                tiltY: tiltY,
                blobRadius: blobRadius,
                badgeSize: badgeSize,
                phase: phase
            )
        }
        .opacity(active ? 1 : 0)
        .animation(.easeInOut(duration: 0.5), value: active)
    }
}

/// Renders one orbit frame at a fixed `phase`.
/// Front badges draw above rear badges.
/// Rear-center badges fade behind the orb.
private struct OrbitingBadgesRing: View {
    let apps: [OrbitApp]
    let radiusX: CGFloat
    let tiltY: CGFloat
    let blobRadius: CGFloat
    let badgeSize: CGFloat
    let phase: Double

    var body: some View {
        ZStack {
            ForEach(Array(apps.enumerated()), id: \.element.id) { index, app in
                let p = orbitBadgePlacement(
                    index: index,
                    count: apps.count,
                    phase: phase,
                    radiusX: radiusX,
                    tiltY: tiltY,
                    blobRadius: blobRadius
                )
                AppBadge(app: app, size: badgeSize)
                    .scaleEffect(p.scale)
                    .blur(radius: p.blur)
                    .opacity(p.opacity)
                    .offset(x: p.offset.width, y: p.offset.height)
                    .zIndex(p.z)
            }
        }
    }
}

/// Renders a bundled app icon or a symbol fallback.
/// Bundled icons are cached by asset name.
private struct AppBadge: View {
    let app: OrbitApp
    /// Sets the tile edge length in points.
    var size: CGFloat = 44

    /// Caches bundled icons by asset name for the process lifetime.
    private static var iconCache: [String: PlatformImage] = [:]

    private static func bundledIcon(named asset: String) -> PlatformImage? {
        if let cached = iconCache[asset] { return cached }
        guard let url = Bundle.module.url(
            forResource: asset, withExtension: "png", subdirectory: "AppIcons"
        ) else { return nil }
        #if canImport(UIKit)
        guard let image = UIImage(contentsOfFile: url.path) else { return nil }
        #else
        guard let image = NSImage(contentsOf: url) else { return nil }
        #endif
        iconCache[asset] = image
        return image
    }

    var body: some View {
        Group {
            if let asset = app.iconAsset, let icon = Self.bundledIcon(named: asset) {
                #if canImport(UIKit)
                Image(uiImage: icon)
                    .resizable()
                    .scaledToFit()
                    .frame(width: size, height: size)
                    .clipShape(RoundedRectangle(cornerRadius: size * 0.25, style: .continuous))
                #else
                Image(nsImage: icon)
                    .resizable()
                    .scaledToFit()
                    .frame(width: size, height: size)
                    .clipShape(RoundedRectangle(cornerRadius: size * 0.25, style: .continuous))
                #endif
            } else {
                RoundedRectangle(cornerRadius: size * 0.25, style: .continuous)
                    .fill(app.color)
                    .frame(width: size, height: size)
                    .overlay(
                        Image(systemName: app.symbol)
                            .font(.system(size: size * 0.5, weight: .semibold))
                            .foregroundStyle(.white)
                    )
                    .overlay(
                        RoundedRectangle(cornerRadius: size * 0.25, style: .continuous)
                            .stroke(.white.opacity(0.25), lineWidth: 0.8)
                    )
            }
        }
        .shadow(color: .black.opacity(0.55), radius: size * 0.14, y: 2)
        .accessibilityLabel("\(app.displayName) involved in this task")
    }
}
