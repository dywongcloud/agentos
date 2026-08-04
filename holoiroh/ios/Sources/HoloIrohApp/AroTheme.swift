import SwiftUI

/// Defines the shared Aro accent and background colors.
extension Color {
    /// Defines the shared Aro orb-blue accent.
    static let aroAccent = Color(red: 0.30, green: 0.56, blue: 1.0)
    /// Defines the brighter accent used for highlights, rings, and gradient tops.
    static let aroAccentBright = Color(red: 0.45, green: 0.70, blue: 1.0)
    /// Defines the near-black background with a blue tint that blends with the accent glow.
    static let aroVoid = Color(red: 0.02, green: 0.03, blue: 0.06)
}

/// Displays the Aro wordmark with a rounded bold font and vertical gradient.
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

/// Displays the compact decorative Aro orb mark.
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

/// Groups ticket and PIN inputs in a frosted card with rounded corners and a gradient border.
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

/// Displays a tracked uppercase field label with a system icon.
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

/// Styles the primary action as a filled accent panel. Disabled buttons use dim neutral colors.
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

/// Styles secondary actions as bordered material buttons.
/// The style sizes to its label.
/// Callers can apply a full-width frame to the label.
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

/// Displays the pairing backdrop with a slowly animated accent glow.
/// The backdrop does not handle input.
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
