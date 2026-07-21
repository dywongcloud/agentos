import SwiftUI

/// The single, canonical "Done" bar that rides above the software keyboard on
/// every text-input surface in the app (the pairing ticket editor + PIN pad in
/// `PairingView`, and the prompt field in `MainView`).
///
/// It exists because iOS gives some keyboards no built-in way off: a multi-line
/// `TextEditor` / `axis: .vertical` `TextField` treats Return as a newline, and
/// the `.numberPad` keyboard has no Return key at all — the classic case from
/// the canonical Stack Overflow answer on adding a Done button to the keyboard
/// (https://stackoverflow.com/questions/10077155). The modern SwiftUI
/// equivalent of the old UIKit `inputAccessoryView` + `UIToolbar` + Done
/// `UIBarButtonItem` recipe is a `ToolbarItemGroup(placement: .keyboard)`.
///
/// Centralizing it here means every keyboard shows an IDENTICAL, accessible,
/// emphasized Done — tuned in ONE place instead of hand-rolled per view, so the
/// experience can never drift between surfaces.
extension View {
    /// Attach the standard app keyboard Done bar.
    ///
    /// - Parameter dismiss: the caller's own focus-clearing action (e.g.
    ///   `focusedField = nil` or `isPromptFocused = false`). Keeping the focus
    ///   model with the caller lets this modifier wrap any `@FocusState` shape
    ///   without owning it — the reason it can be shared across every view.
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

/// Shared styling for the keyboard Done bar, kept out of the `View` extension so
/// the modifier (and any future caller) references one accent value.
enum KeyboardDoneToolbar {
    /// The app's orb-blue brand accent, matched to `PairingView`/`MainView`'s
    /// local `orbAccent` so Done is on-brand and identical on every keyboard.
    static let accent = Color(red: 0.30, green: 0.56, blue: 1.0)
}
