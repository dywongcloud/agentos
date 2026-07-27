import SwiftUI

struct ContentView: View {
    private enum Route: Hashable {
        case main(ticket: String, pin: String)
    }

    @State private var path: [Route] = []

    @State private var isIntroPlaying = true

    @EnvironmentObject private var profileStore: ConnectionProfileStore

    @StateObject private var reachability = ReachabilityMonitor(ticket: "")

    @AppStorage(AppSettings.AutoConnect.storageKey)
    private var autoConnectEnabled = AppSettings.AutoConnect.enabledByDefault

    @State private var didAttemptAutoConnect = false
    @State private var userEngagedPairing = false
    @State private var manualDisconnectThisSession = false

    @State private var showDiagnostics = false

    private static var debugAutoPairFromEnvironment: (ticket: String, pin: String)? {
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
        ZStack {
            NavigationStack(path: $path) {
                PairingView(onConnect: { ticket, pin in
                    profileStore.markConnected(ticket: ticket)
                    path.append(.main(ticket: ticket, pin: pin))
                }, onInteract: {
                    userEngagedPairing = true
                })
                .environmentObject(reachability)
                .navigationDestination(for: Route.self) { route in
                    switch route {
                    case .main(let ticket, let pin):
                        MainView(ticket: ticket, pin: pin) {
                            manualDisconnectThisSession = true
                            path.removeAll()
                        }
                        .environmentObject(profileStore)
                    }
                }
            }
            .onAppear {
                startReachability()
                guard path.isEmpty, let auto = Self.debugAutoPairFromEnvironment else { return }
                path.append(.main(ticket: auto.ticket, pin: auto.pin))
            }
            .onChange(of: profileStore.autoConnectProfile?.ticket) { _, newTicket in
                reachability.ticket = newTicket ?? ""
                reachability.checkNow()
            }
            .onChange(of: reachability.state) { _, _ in
                autoConnectIfAllowed()
            }

            if isIntroPlaying {
                IntroView {
                    Haptics.fire(.introReveal)
                    withAnimation(.easeInOut(duration: 0.6)) {
                        isIntroPlaying = false
                    }
                    DispatchQueue.main.async { autoConnectIfAllowed() }
                }
                .transition(.opacity.combined(with: .scale(scale: 1.08, anchor: .top)))
                .zIndex(1)
            }
        }
        .onShake { showDiagnostics = true }
        .sheet(isPresented: $showDiagnostics) {
            DiagnosticsView()
                .environmentObject(profileStore)
                .environmentObject(reachability)
        }
    }

    private func startReachability() {
        if let ticket = profileStore.autoConnectProfile?.ticket, reachability.ticket != ticket {
            reachability.ticket = ticket
        }
        reachability.start()
    }

    private var userHasNotTakenOverPairing: Bool {
        !userEngagedPairing && !manualDisconnectThisSession
    }

    private var isShowingPairingScreen: Bool {
        path.isEmpty && !isIntroPlaying
    }

    private func autoConnectIfAllowed() {
        guard autoConnectEnabled,
              !didAttemptAutoConnect,
              userHasNotTakenOverPairing,
              isShowingPairingScreen,
              Self.debugAutoPairFromEnvironment == nil,
              reachability.state == .reachable,
              let target = profileStore.autoConnectProfile
        else { return }
        didAttemptAutoConnect = true
        ConnectionDiagnostics.shared.note("auto-connect: \(target.name) reachable -> opening session")
        Haptics.fire(.connect)
        profileStore.markConnected(ticket: target.ticket)
        path.append(.main(ticket: target.ticket, pin: target.pin))
    }
}

#Preview {
    ContentView()
        .environmentObject(ConnectionProfileStore())
}
