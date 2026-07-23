import SwiftUI

/// A `PreferenceKey` for reading a view's real rendered height back up the view
/// tree, via `.background(GeometryReader { ... }.preference(key: ViewHeightKey.self, value: geo.size.height))`
/// paired with `.onPreferenceChange(ViewHeightKey.self) { height in ... }` on an
/// ancestor. Used where a layout needs to react to another view's ACTUAL
/// measured size instead of a guessed constant (see `MainView`'s command-bar
/// keyboard-avoidance math, which used to hardcode a height estimate that
/// silently went stale as more content -- the clarify panel, the recent-prompts
/// strip -- was added above the command bar).
struct ViewHeightKey: PreferenceKey {
    static let defaultValue: CGFloat = 0
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) {
        value = nextValue()
    }
}
