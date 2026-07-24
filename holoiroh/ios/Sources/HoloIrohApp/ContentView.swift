import SwiftUI

/// Root view: hosts the `NavigationStack` that moves the user from
/// `PairingView` to `MainView` on "connect", per holoiroh/README.md's
/// iOS-side architecture (Pairing -> Live view / Prompts / Status).
///
/// Navigation state is a simple `[Route]` path rather than a boolean --
/// using `NavigationStack(path:)` (iOS 16+, well within this package's
/// iOS 17 minimum) keeps the door open for deeper stacks later (e.g. a
/// settings screen) without another rewrite of the navigation wiring.
/// There is still no real iroh/control-channel connection here: "connect"
/// only means "the user supplied a ticket and pushed past pairing." Voice
/// transcription (`VoiceTranscriberModel`) is wired into `MainView`'s mic
/// button, not here, since that's where the real prompt field lives.
struct ContentView: View {
    private enum Route: Hashable {
        case main(ticket: String, pin: String)
    }

    @State private var path: [Route] = []

    /// The brand intro plays on EVERY launch (product decision: the animated
    /// Aro reveal is the entry moment every time the app opens, not a one-time
    /// new-user gate). It stays fully skippable -- a tap anywhere or the Skip
    /// button ends it early, and it honors reduce-motion -- so replaying it on
    /// each open is never a forced wait.
    @State private var showIntro = true

    /// The app-wide profile store (injected by `HoloIrohApp`), re-injected into
    /// the diagnostics sheet.
    @EnvironmentObject private var profileStore: ConnectionProfileStore

    /// Live "is the Dev Mac daemon reachable?" signal, owned here so both the
    /// pairing-screen status pill and the launch auto-connect read one instance.
    /// Probes the default profile's ticket (set + started in `.onAppear`).
    @StateObject private var reachability = ReachabilityMonitor(ticket: "")

    /// Auto-connect the default profile on launch when it's reachable. User
    /// opt-out (Diagnostics → Settings); default on.
    @AppStorage("autoConnectEnabled") private var autoConnectEnabled = true

    /// Auto-connect fires at most once per launch, and never once the user has
    /// engaged the pairing screen by hand or explicitly disconnected a session.
    @State private var didAttemptAutoConnect = false
    @State private var userEngagedPairing = false
    @State private var manualDisconnectThisSession = false

    /// Auto-connect used to fire in the SAME FRAME the intro finished fading (whenever the
    /// daemon was already reachable, which is now virtually every launch since the default
    /// profile is auto-ensured) -- `PairingView` is fully built (QR scan, ticket+PIN entry,
    /// phrase verification, a complete sqlite-backed saved-profiles list) but was composited
    /// for zero perceptible frames before `MainView` replaced it, so it read as "missing" even
    /// though the code and data were correct. This flag gates BOTH `maybeAutoConnect()` call
    /// sites and only flips true a beat after the intro reveals the pairing screen, so the user
    /// genuinely sees it exists before any automatic navigation away from it.
    @State private var autoConnectGraceElapsed = false
    /// How long `PairingView` must be visibly on-screen after the intro reveals it before
    /// auto-connect is allowed to navigate away -- long enough to register as a real screen,
    /// short enough that a returning user reaching for a known-reachable Dev Mac still feels
    /// instant.
    private static let autoConnectGracePeriod: TimeInterval = 0.6

    /// The hidden diagnostics screen, opened by shaking the device (or the
    /// long-press affordance on the pairing header).
    @State private var showDiagnostics = false

    /// Debug-only auto-pair bypass: when both env vars are set (e.g. via
    /// `devicectl device process launch --environment-variables` for an
    /// unattended device witness, matching this project's own
    /// `holoiroh-redeploy-iphone-fix` verification step), skip the manual
    /// QR-scan/paste UI entirely and jump straight to `MainView` with the
    /// supplied ticket/PIN, exactly as if the user had pasted them in
    /// `PairingView`. Never read outside `DEBUG` -- this must not become a
    /// real unauthenticated pairing bypass in a shipped build.
    private static var autoPairFromEnvironment: (ticket: String, pin: String)? {
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
                            // A manual disconnect stands the launch auto-connect
                            // down for the rest of this session (no reconnect loop).
                            manualDisconnectThisSession = true
                            path.removeAll()
                        }
                        .environmentObject(profileStore)
                    }
                }
            }
            .onAppear {
                startReachability()
                guard path.isEmpty, let auto = Self.autoPairFromEnvironment else { return }
                path.append(.main(ticket: auto.ticket, pin: auto.pin))
            }
            // The auto-connect target's ticket can change at runtime (Group: ticket refresh,
            // or a fresh profile just became the last-connected one) -- keep the monitor
            // pointed at whichever profile auto-connect would actually dial, not always the
            // synthesized Dev Mac default, so the reachability gate matches the real target.
            .onChange(of: profileStore.autoConnectProfile?.ticket) { _, newTicket in
                reachability.ticket = newTicket ?? ""
                reachability.checkNow()
            }
            // Auto-connect the moment the daemon is confirmed reachable (guards
            // handle intro/engagement/once-per-launch).
            .onChange(of: reachability.state) { _, _ in
                maybeAutoConnect()
            }

            // The intro plays over the (already-loaded) pairing screen, then
            // scales/fades away to reveal it -- one continuous motion, so the
            // orb the intro built up hands off into the pairing header glow.
            if showIntro {
                IntroView {
                    Haptics.fire(.introReveal)
                    withAnimation(.easeInOut(duration: 0.6)) {
                        showIntro = false
                    }
                    // Reveal hands off to the live session once the pairing screen has had a
                    // real moment on-screen -- not the instant the intro finishes fading (see
                    // `autoConnectGraceElapsed`'s doc).
                    DispatchQueue.main.asyncAfter(deadline: .now() + Self.autoConnectGracePeriod) {
                        autoConnectGraceElapsed = true
                        maybeAutoConnect()
                    }
                }
                .transition(.opacity.combined(with: .scale(scale: 1.08, anchor: .top)))
                .zIndex(1)
            }
        }
        // Shake (or long-press the "Aro" title) opens the hidden diagnostics
        // screen -- see DiagnosticsView. Both post the same notification.
        .onShake { showDiagnostics = true }
        .sheet(isPresented: $showDiagnostics) {
            DiagnosticsView()
                .environmentObject(profileStore)
                .environmentObject(reachability)
        }
    }

    /// Points the monitor at the current auto-connect target's ticket and begins periodic
    /// probing. Safe to call repeatedly (start() is an idempotent restart).
    private func startReachability() {
        if let ticket = profileStore.autoConnectProfile?.ticket, reachability.ticket != ticket {
            reachability.ticket = ticket
        }
        reachability.start()
    }

    /// Auto-connect the default profile iff every guardrail is satisfied:
    /// enabled, not already attempted this launch, the user hasn't engaged the
    /// pairing screen or manually disconnected, the intro is done, we're on the
    /// pairing screen (empty nav path), the debug auto-pair isn't driving, and
    /// the daemon is confirmed reachable.
    private func maybeAutoConnect() {
        guard autoConnectEnabled,
              autoConnectGraceElapsed,
              !didAttemptAutoConnect,
              !userEngagedPairing,
              !manualDisconnectThisSession,
              !showIntro,
              path.isEmpty,
              Self.autoPairFromEnvironment == nil,
              reachability.state == .reachable,
              let def = profileStore.autoConnectProfile
        else { return }
        didAttemptAutoConnect = true
        ConnectionDiagnostics.shared.note("auto-connect: \(def.name) reachable -> opening session")
        Haptics.fire(.connect)
        profileStore.markConnected(ticket: def.ticket)
        path.append(.main(ticket: def.ticket, pin: def.pin))
    }
}

#Preview {
    ContentView()
        .environmentObject(ConnectionProfileStore())
}
