import Combine
import Foundation

#if canImport(HoloirohIosBridge)
import HoloirohIosBridge
#endif

/// Live "is the daemon reachable?" signal for a single profile's ticket, driving
/// the pairing screen's status pill and the launch auto-connect decision. Calls
/// the ADDITIVE `holoiroh_ios_bridge_probe_reachable` FFI off the main thread on
/// a serial queue and publishes `.reachable`/`.unreachable`/`.checking` on main.
///
/// Deliberately cheap and side-effect-free: the probe binds its OWN throwaway
/// iroh endpoint (never the live connection's bridge), so a probe can never
/// disturb an in-progress or established connect. At most one probe is in flight
/// at a time (`checkNow` is a no-op while one runs); a `generation` token drops
/// stale results after `stop()`/`start()`, so a slow probe returning after the
/// view disappeared can't publish onto a restarted monitor.
///
/// In the bridge-less build (`#if !canImport(HoloirohIosBridge)` -- the headless
/// macOS `swift build`/CI), the probe can't run: `state` stays `.unknown` and
/// the monitor never spins, so the UI shows a neutral (not false-negative) dot.
@MainActor
final class ReachabilityMonitor: ObservableObject {

    enum Reachability: Equatable {
        /// Not yet probed (initial state, and the permanent state in a
        /// bridge-less build). UI: neutral/gray, no claim either way.
        case unknown
        /// A probe is in flight. UI: pulsing.
        case checking
        /// The daemon answered the control dial within the timeout.
        case reachable
        /// The probe timed out or failed (daemon down / ticket dead / offline).
        case unreachable
    }

    @Published private(set) var state: Reachability = .unknown
    @Published private(set) var lastCheckedAt: Date?

    /// The ticket this monitor probes. Mutable so the pairing screen can point a
    /// single monitor at whichever profile is the current default (e.g. after a
    /// ticket refresh) without recreating it; a change resets state to `.unknown`.
    var ticket: String {
        didSet {
            guard ticket != oldValue else { return }
            state = .unknown
            lastCheckedAt = nil
        }
    }

    private let timeoutMs: UInt64
    private let probeQueue = DispatchQueue(label: "com.holoiroh.reachability", qos: .utility)

    /// True while a probe runs, so overlapping `checkNow()` calls collapse to one.
    private var inFlight = false

    /// Bumped by `stop()`/`start()`/ticket change; an async probe result only
    /// publishes if its captured generation still matches (cancelation).
    private var generation = 0

    private var pollTimer: Timer?

    init(ticket: String, timeoutMs: UInt64 = 4000) {
        self.ticket = ticket
        self.timeoutMs = timeoutMs
    }

    /// Begin periodic probing (an immediate first probe, then every `interval`
    /// seconds). Idempotent restart: cancels any prior schedule + stale probe.
    func start(interval: TimeInterval = 20) {
        generation += 1
        pollTimer?.invalidate()
        checkNow()
        let timer = Timer(timeInterval: interval, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.checkNow() }
        }
        RunLoop.main.add(timer, forMode: .common)
        pollTimer = timer
    }

    /// Stop probing and drop any in-flight result (via the generation bump).
    func stop() {
        generation += 1
        pollTimer?.invalidate()
        pollTimer = nil
        inFlight = false
    }

    /// Fire a single probe now, unless one is already running. Safe to call from
    /// `.onAppear`, a manual refresh button, or the poll timer.
    func checkNow() {
        guard !inFlight else { return }
        let ticket = self.ticket
        guard !ticket.isEmpty else {
            state = .unknown
            return
        }

        #if canImport(HoloirohIosBridge)
        inFlight = true
        state = .checking
        let timeoutMs = self.timeoutMs
        let myGeneration = generation
        probeQueue.async { [weak self] in
            let reachable = ticket.withCString { cstr in
                holoiroh_ios_bridge_probe_reachable(cstr, timeoutMs)
            }
            Task { @MainActor in
                guard let self, self.generation == myGeneration else { return }
                self.inFlight = false
                self.state = reachable ? .reachable : .unreachable
                self.lastCheckedAt = Date()
            }
        }
        #else
        state = .unknown
        #endif
    }
}
