import SwiftUI

/// App entry point for the HoloIroh iOS client.
///
/// Intended responsibilities (see holoiroh/README.md for the full
/// architecture): pair with the Mac daemon via an iroh ticket, render the
/// live screen+audio stream, and send text/voice prompts over the
/// bidirectional control channel so they can be bridged to
/// `holo-desktop-cli` on the Mac side.
///
/// This is currently a skeleton -- pairing, streaming, and the control
/// channel are not wired up yet.
@main
struct HoloIrohApp: App {
    /// The single, app-wide connection-profile store. Created HERE (not inside
    /// PairingView) so the default "Dev Mac" profile -- the current daemon's
    /// iroh ticket + PIN -- is seeded the instant the app launches, guaranteed
    /// and independent of any view's lifecycle (the always-on intro overlay,
    /// the NavigationStack root's `@StateObject` init timing, etc.). Both
    /// `PairingView` reads it via `@EnvironmentObject`, so the always-present
    /// "Dev Mac" default and any user-saved profiles share one store instance
    /// and one sqlite file.
    @StateObject private var profileStore = ConnectionProfileStore()

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(profileStore)
        }
    }
}
