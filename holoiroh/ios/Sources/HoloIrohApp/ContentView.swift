import SwiftUI

/// Root view: hosts the `NavigationStack` that moves the user from
/// `PairingView` to `MainView` on "connect", per holoiroh/README.md's
/// iOS-side architecture (Pairing -> Live view / Prompts / Status).
///
/// Navigation state is a simple `[Route]` path rather than a boolean --
/// using `NavigationStack(path:)` (iOS 16+, well within this package's
/// iOS 17 minimum) keeps the door open for deeper stacks later (e.g. a
/// settings screen) without another rewrite of the navigation wiring.
/// There is still no real iroh/control-channel connection here: "connect"
/// only means "the user supplied a ticket and pushed past pairing." Voice
/// transcription (`VoiceTranscriberModel`) is wired into `MainView`'s mic
/// button, not here, since that's where the real prompt field lives.
struct ContentView: View {
    private enum Route: Hashable {
        case main(ticket: String, pin: String)
    }

    @State private var path: [Route] = []

    /// Debug-only auto-pair bypass: when both env vars are set (e.g. via
    /// `devicectl device process launch --environment-variables` for an
    /// unattended device witness, matching this project's own
    /// `holoiroh-redeploy-iphone-fix` verification step), skip the manual
    /// QR-scan/paste UI entirely and jump straight to `MainView` with the
    /// supplied ticket/PIN, exactly as if the user had pasted them in
    /// `PairingView`. Never read outside `DEBUG` -- this must not become a
    /// real unauthenticated pairing bypass in a shipped build.
    private static var autoPairFromEnvironment: (ticket: String, pin: String)? {
        #if DEBUG
        let env = ProcessInfo.processInfo.environment
        guard let ticket = env["HOLOIROH_AUTOPAIR_TICKET"], !ticket.isEmpty,
              let pin = env["HOLOIROH_AUTOPAIR_PIN"], !pin.isEmpty
        else { return nil }
        return (ticket, pin)
        #else
        return nil
        #endif
    }

    var body: some View {
        NavigationStack(path: $path) {
            PairingView { ticket, pin in
                path.append(.main(ticket: ticket, pin: pin))
            }
            .navigationDestination(for: Route.self) { route in
                switch route {
                case .main(let ticket, let pin):
                    MainView(ticket: ticket, pin: pin) {
                        path.removeAll()
                    }
                }
            }
        }
        .onAppear {
            guard path.isEmpty, let auto = Self.autoPairFromEnvironment else { return }
            path.append(.main(ticket: auto.ticket, pin: auto.pin))
        }
    }
}

#Preview {
    ContentView()
}
