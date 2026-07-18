import SwiftUI

/// Placeholder root view. Will become the pairing / live-view / prompt
/// screen described in holoiroh/README.md.
struct ContentView: View {
    var body: some View {
        VStack(spacing: 12) {
            Text("HoloIroh")
                .font(.largeTitle)
                .fontWeight(.bold)
            Text("Skeleton -- pairing and live view not yet implemented")
                .font(.footnote)
                .foregroundStyle(.secondary)
        }
        .padding()
    }
}

#Preview {
    ContentView()
}
