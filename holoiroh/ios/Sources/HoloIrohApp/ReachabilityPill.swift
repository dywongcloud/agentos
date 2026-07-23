import SwiftUI

/// Compact live status dot + label for a profile's daemon reachability, driven
/// by `ReachabilityMonitor.Reachability`. Green "reachable", red "offline",
/// a pulsing dot while "checking", and a neutral gray when unknown (bridge-less
/// build or not-yet-probed) so it never makes a false claim either way.
struct ReachabilityPill: View {
    let state: ReachabilityMonitor.Reachability
    var showsLabel: Bool = true

    @State private var pulse = false

    private var color: Color {
        switch state {
        case .reachable: return Color(red: 0.30, green: 0.85, blue: 0.46)
        case .unreachable: return Color(red: 0.95, green: 0.42, blue: 0.38)
        case .checking: return Color.aroAccentBright
        case .unknown: return Color.white.opacity(0.35)
        }
    }

    private var label: String {
        switch state {
        case .reachable: return "reachable"
        case .unreachable: return "offline"
        case .checking: return "checking…"
        case .unknown: return "unknown"
        }
    }

    var body: some View {
        HStack(spacing: 6) {
            Circle()
                .fill(color)
                .frame(width: 8, height: 8)
                .overlay(
                    Circle()
                        .stroke(color.opacity(0.5), lineWidth: 4)
                        .scaleEffect(pulse && state == .checking ? 1.8 : 1.0)
                        .opacity(pulse && state == .checking ? 0.0 : 0.6)
                )
                .shadow(color: color.opacity(0.7), radius: state == .reachable ? 4 : 0)
            if showsLabel {
                Text(label)
                    .font(.caption2.weight(.medium))
                    .foregroundStyle(color)
            }
        }
        .onAppear {
            withAnimation(.easeOut(duration: 0.9).repeatForever(autoreverses: false)) {
                pulse = true
            }
        }
        .accessibilityLabel("Daemon \(label)")
    }
}
