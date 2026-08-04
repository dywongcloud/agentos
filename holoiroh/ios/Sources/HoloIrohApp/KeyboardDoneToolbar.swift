import SwiftUI

/// Provides the standard Done toolbar above app software keyboards.
/// The toolbar supports multiline fields and number pads that have no dismissal key.
/// All callers use the same action, styling, and accessibility metadata.
extension View {
    /// Attaches the standard keyboard Done toolbar.
    ///
    /// - Parameter dismiss: Clears the caller-owned focus state.
    func keyboardDoneToolbar(dismiss: @escaping () -> Void) -> some View {
        toolbar {
            ToolbarItemGroup(placement: .keyboard) {
                // Trailing placement is the iOS-standard spot for the primary
                // keyboard-toolbar action; the Spacer pushes Done to the right.
                Spacer()
                Button(action: dismiss) {
                    // Semibold + on-brand accent so Done reads as a first-class,
                    // emphasized action rather than incidental chrome — the UX
                    // optimization over a bare default-weight `Button("Done")`.
                    Text("Done").fontWeight(.semibold)
                }
                .tint(KeyboardDoneToolbar.accent)
                // VoiceOver: announce not just the title but what it does, and
                // give the control a stable identifier for UI witnesses.
                .accessibilityLabel("Done")
                .accessibilityHint("Dismisses the keyboard")
                .accessibilityIdentifier("keyboardDoneButton")
            }
        }
    }
}

/// Provides shared styling for the keyboard Done toolbar.
enum KeyboardDoneToolbar {
    /// Defines the shared orb-blue accent for the Done action.
    static let accent = Color(red: 0.30, green: 0.56, blue: 1.0)
}
