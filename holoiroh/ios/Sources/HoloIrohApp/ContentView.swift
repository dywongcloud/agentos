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
        case main(ticket: String)
    }

    @State private var path: [Route] = []

    var body: some View {
        NavigationStack(path: $path) {
            PairingView { ticket in
                path.append(.main(ticket: ticket))
            }
            .navigationDestination(for: Route.self) { route in
                switch route {
                case .main(let ticket):
                    MainView(ticket: ticket) {
                        path.removeAll()
                    }
                }
            }
        }
    }
}

#Preview {
    ContentView()
}
