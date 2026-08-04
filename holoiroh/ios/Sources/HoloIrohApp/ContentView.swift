import SwiftUI

struct ContentView: View {
    private enum Route: Hashable {
        case main(ticket: String, pin: String)
    }

    @State private var path: [Route] = []

    @State private var isIntroPlaying = true

    @EnvironmentObject private var profileStore: ConnectionProfileStore

    @StateObject private var reachability = ReachabilityMonitor(ticket: "")
    @StateObject private var tinfoilVerificationStore = TinfoilVerificationStore()

    @AppStorage(AppSettings.AutoConnect.storageKey)
    private var autoConnectEnabled = AppSettings.AutoConnect.enabledByDefault

    @State private var didAttemptAutoConnect = false
    @State private var userEngagedPairing = false
    @State private var manualDisconnectThisSession = false

    @State private var showDiagnostics = false
    @State private var identityErrorAlert: String?

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

    @ViewBuilder
    var body: some View {
        #if DEBUG && canImport(UIKit)
        if ProcessInfo.processInfo.environment["HOLOIROH_WITNESS_GESTURE_SURFACE"] == "1" {
            GestureWitnessSurface()
        } else {
            appBody
        }
        #else
        appBody
        #endif
    }

    private var appBody: some View {
        ZStack {
            NavigationStack(path: $path) {
                PairingView(onConnect: { ticket, pin in
                    profileStore.markConnected(ticket: ticket)
                    openMain(ticket: ticket, pin: pin)
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
                        .environmentObject(tinfoilVerificationStore)
                    }
                }
            }
            .onAppear {
                startReachability()
                if let identityError = reachability.identityErrorDescription {
                    identityErrorAlert = identityError
                }
                guard path.isEmpty, let auto = Self.debugAutoPairFromEnvironment else { return }
                openMain(ticket: auto.ticket, pin: auto.pin)
            }
            .onChange(of: path) { _, newPath in
                if newPath.isEmpty {
                    startReachability()
                } else {
                    reachability.stop()
                }
            }
            .onChange(of: profileStore.autoConnectProfile?.ticket) { _, newTicket in
                reachability.ticket = newTicket ?? ""
                reachability.checkNow()
            }
            .onChange(of: reachability.state) { _, _ in
                autoConnectIfAllowed()
            }
            .onChange(of: reachability.identityErrorDescription) { _, newError in
                identityErrorAlert = newError
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
                .environmentObject(tinfoilVerificationStore)
        }
        .alert(
            "Iroh Identity Unavailable",
            isPresented: Binding(
                get: { identityErrorAlert != nil },
                set: { if !$0 { identityErrorAlert = nil } }
            )
        ) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(identityErrorAlert ?? "The persistent iOS identity could not be loaded.")
        }
    }

    private func startReachability() {
        guard path.isEmpty else { return }
        if let ticket = profileStore.autoConnectProfile?.ticket, reachability.ticket != ticket {
            reachability.ticket = ticket
        }
        reachability.start()
    }

    private func openMain(ticket: String, pin: String) {
        reachability.stop()
        path.append(.main(ticket: ticket, pin: pin))
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
        openMain(ticket: target.ticket, pin: target.pin)
    }
}

#if DEBUG && canImport(UIKit)
private struct GestureWitnessSurface: View {
    @State private var zoomScale: CGFloat = 1
    @State private var panOffset: CGSize = .zero
    @State private var frameSource: VideoFrameSource = SyntheticVideoFrameSource()

    var body: some View {
        GeometryReader { geometry in
            PanZoomVideoSurface(
                frameSource: frameSource,
                viewport: geometry.size,
                isVideoFullscreen: true,
                isControllingRemotely: false,
                zoomScale: $zoomScale,
                panOffset: $panOffset
            )
            .accessibilityLabel("Live remote view of the Mac")
        }
        .background(Color.black)
    }
}
#endif

#Preview {
    ContentView()
        .environmentObject(ConnectionProfileStore())
}
