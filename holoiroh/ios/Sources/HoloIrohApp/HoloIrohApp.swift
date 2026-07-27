import SwiftUI

@main
struct HoloIrohApp: App {
    @StateObject private var profileStore = ConnectionProfileStore()

    init() {
        AppSettings.AutoConnect.applyOptInDefaultOnce()
    }

    var body: some Scene {
        WindowGroup {
            rootView
                .environmentObject(profileStore)
        }
    }

    @ViewBuilder
    private var rootView: some View {
        if let recentPromptContainer = RecentPromptStore.container {
            ContentView().modelContainer(recentPromptContainer)
        } else {
            ContentView()
        }
    }
}
