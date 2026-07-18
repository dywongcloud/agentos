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
    var body: some Scene {
        WindowGroup {
            ContentView()
        }
    }
}
