import SwiftUI

/// Shared Aro visual identity: the one brand accent blue (single-sourced here
/// so the intro, the pairing screen, and the orb reaction effects all draw the
/// exact same hue instead of each re-hardcoding `Color(red: 0.30, ...)`), plus
/// the reusable building blocks the pairing screen is composed from -- the
/// wordmark, the frosted input card, the living backdrop, and the button
/// styles.
///
/// Everything here is pure SwiftUI (no UIKit), on purpose: the same components
/// that render on device also render headlessly via `ImageRenderer` on macOS,
/// which is exactly what the pixel-witness harness for this redesign renders.
extension Color {
    /// The Aro orb blue -- the single brand accent, matching the Spline blob
    /// and the orb reaction effects (`OrbEffects.swift`).
    static let aroAccent = Color(red: 0.30, green: 0.56, blue: 1.0)
    /// A brighter tint of the accent for highlights, rings and gradient tops.
    static let aroAccentBright = Color(red: 0.45, green: 0.70, blue: 1.0)
    /// The deep near-black field the dark UI sits on (a hair of blue so the
    /// accent glow blends instead of banding against pure `.black`).
    static let aroVoid = Color(red: 0.02, green: 0.03, blue: 0.06)
}

/// The "Aro" wordmark: rounded-bold with a white -> accent vertical gradient.
/// One definition, reused by the intro's resting state and the pairing header.
struct AroWordmark: View {
    var size: CGFloat = 40
    var body: some View {
        Text("Aro")
            .font(.system(size: size, weight: .bold, design: .rounded))
            .foregroundStyle(
                LinearGradient(colors: [.white, .aroAccent], startPoint: .top, endPoint: .bottom)
            )
            .accessibilityAddTraits(.isHeader)
    }
}

/// A small self-contained glowing orb mark -- the brand's compact form, tying
/// the pairing header back to the intro's orb. Purely decorative.
struct AroOrbMark: View {
    var diameter: CGFloat = 54
    var body: some View {
        ZStack {
            Circle()
                .fill(
                    RadialGradient(
                        colors: [Color.aroAccent.opacity(0.5), .clear],
                        center: .center, startRadius: 0, endRadius: diameter * 0.9
                    )
                )
                .frame(width: diameter * 1.9, height: diameter * 1.9)
                .blur(radius: 8)
            Circle()
                .fill(
                    RadialGradient(
                        colors: [.white, .aroAccentBright, .aroAccent, .aroAccent.opacity(0)],
                        center: UnitPoint(x: 0.42, y: 0.38), startRadius: 0, endRadius: diameter * 0.62
                    )
                )
                .frame(width: diameter, height: diameter)
        }
        .allowsHitTesting(false)
    }
}

/// A frosted-glass grouping card: ultraThinMaterial with a soft hairline
/// gradient border and continuous rounded corners -- the container the ticket
/// and PIN inputs live in, giving the screen its premium, high-tech feel.
struct AroCard<Content: View>: View {
    var cornerRadius: CGFloat = 16
    @ViewBuilder var content: Content
    var body: some View {
        content
            .padding(14)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                .ultraThinMaterial,
                in: RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
            )
            .overlay(
                RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
                    .strokeBorder(
                        LinearGradient(
                            colors: [.white.opacity(0.16), .white.opacity(0.04)],
                            startPoint: .top, endPoint: .bottom
                        ),
                        lineWidth: 1
                    )
            )
    }
}

/// A small accent section label (e.g. "IROH TICKET") -- icon + tracked caps.
struct AroFieldLabel: View {
    var title: String
    var systemImage: String
    var body: some View {
        Label(title, systemImage: systemImage)
            .font(.caption2.weight(.semibold))
            .textCase(.uppercase)
            .tracking(1.1)
            .foregroundStyle(Color.aroAccentBright)
            .accessibilityLabel(title)
    }
}

/// Primary action button: a filled, rounded, accent panel with a soft glow --
/// the visual anchor for "Connect". Fills its slot; dims when disabled.
struct AroPrimaryButtonStyle: ButtonStyle {
    var enabled: Bool = true
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.headline)
            .foregroundStyle(.white)
            .frame(maxWidth: .infinity)
            .padding(.vertical, 15)
            .background(
                RoundedRectangle(cornerRadius: 15, style: .continuous)
                    .fill(
                        LinearGradient(
                            colors: enabled
                                ? [Color.aroAccentBright, Color.aroAccent]
                                : [Color.white.opacity(0.16), Color.white.opacity(0.10)],
                            startPoint: .top, endPoint: .bottom
                        )
                    )
            )
            .shadow(
                color: enabled ? Color.aroAccent.opacity(configuration.isPressed ? 0.2 : 0.45) : .clear,
                radius: configuration.isPressed ? 6 : 14, y: 4
            )
            .scaleEffect(configuration.isPressed ? 0.98 : 1.0)
            .opacity(enabled ? 1.0 : 0.7)
            .animation(.easeOut(duration: 0.15), value: configuration.isPressed)
    }
}

/// Secondary action button: a glass/bordered pill for less-prominent actions
/// (Scan QR, Save). Sizes to its label -- add `.frame(maxWidth: .infinity)` on
/// the label for a full-width variant.
struct AroSecondaryButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.subheadline.weight(.semibold))
            .foregroundStyle(.white.opacity(0.92))
            .padding(.vertical, 13)
            .padding(.horizontal, 18)
            .background(
                .ultraThinMaterial,
                in: RoundedRectangle(cornerRadius: 14, style: .continuous)
            )
            .overlay(
                RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .strokeBorder(Color.white.opacity(0.12), lineWidth: 1)
            )
            .opacity(configuration.isPressed ? 0.7 : 1.0)
            .animation(.easeOut(duration: 0.15), value: configuration.isPressed)
    }
}

/// The pairing screen's living backdrop: a deep near-black field with a slow,
/// gentle orb-accent glow breathing near the top -- consistent with the intro
/// orb so the two screens feel like one product. Non-interactive; the pairing
/// view layers its own tap-to-dismiss gesture over this.
struct PairingBackdrop: View {
    @State private var breathe = false
    var body: some View {
        ZStack(alignment: .top) {
            Color.aroVoid
            RadialGradient(
                colors: [Color.aroAccent.opacity(0.30), Color.aroAccent.opacity(0.07), .clear],
                center: .init(x: 0.5, y: 0.08),
                startRadius: 0,
                endRadius: breathe ? 540 : 440
            )
            .opacity(breathe ? 1.0 : 0.8)
            .blur(radius: 6)
        }
        .ignoresSafeArea()
        .onAppear {
            withAnimation(.easeInOut(duration: 4.5).repeatForever(autoreverses: true)) {
                breathe = true
            }
        }
    }
}
