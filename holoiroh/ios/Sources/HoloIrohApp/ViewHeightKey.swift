import SwiftUI

/// Sends a rendered view height to an ancestor through SwiftUI preferences.
/// Use this key when another view's measured height controls layout.
struct ViewHeightKey: PreferenceKey {
    static let defaultValue: CGFloat = 0
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) {
        value = nextValue()
    }
}
