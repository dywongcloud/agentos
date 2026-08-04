import Combine
import Foundation

#if canImport(HoloirohIosBridge)
import HoloirohIosBridge
#endif

@MainActor
final class ReachabilityMonitor: ObservableObject {
    enum Reachability: Equatable {
        case unknown
        case checking
        case reachable
        case unreachable
    }

    @Published private(set) var state: Reachability = .unknown
    @Published private(set) var lastCheckedAt: Date?
    @Published private(set) var identityErrorDescription: String?

    var ticket: String {
        didSet {
            guard ticket != oldValue else { return }
            generation += 1
            inFlight = false
            state = .unknown
            lastCheckedAt = nil
        }
    }

    private let timeoutMs: UInt64
    private let secretKey: Data?
    private let probeQueue = DispatchQueue(label: "com.holoiroh.reachability", qos: .utility)
    private let probeQueueKey = DispatchSpecificKey<UInt8>()
    private var inFlight = false
    private var generation = 0
    private var isActive = false
    private var pollTimer: Timer?

    init(
        ticket: String,
        timeoutMs: UInt64 = 4000,
        identityStore: IrohIdentityStore = IrohIdentityStore()
    ) {
        self.ticket = ticket
        self.timeoutMs = timeoutMs
        do {
            self.secretKey = try identityStore.loadOrCreateSeed()
            self.identityErrorDescription = nil
        } catch {
            self.secretKey = nil
            self.identityErrorDescription = error.localizedDescription
        }
        probeQueue.setSpecific(key: probeQueueKey, value: 1)
    }

    func start(interval: TimeInterval = 20) {
        generation += 1
        isActive = true
        pollTimer?.invalidate()
        checkNow()
        let timer = Timer(timeInterval: interval, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.checkNow() }
        }
        RunLoop.main.add(timer, forMode: .common)
        pollTimer = timer
    }

    func stop() {
        generation += 1
        isActive = false
        pollTimer?.invalidate()
        pollTimer = nil
        inFlight = false
        if DispatchQueue.getSpecific(key: probeQueueKey) == nil {
            probeQueue.sync {}
        }
    }

    func checkNow() {
        guard isActive, !inFlight else { return }
        guard let secretKey else {
            state = .unknown
            return
        }
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
            let reachable = IrohEndpointCoordinator.shared.withLease {
                secretKey.withUnsafeBytes { keyBytes in
                    ticket.withCString { ticketCString in
                        holoiroh_ios_bridge_probe_reachable_with_secret_key(
                            ticketCString,
                            timeoutMs,
                            keyBytes.bindMemory(to: UInt8.self).baseAddress,
                            UInt(keyBytes.count)
                        )
                    }
                }
            }
            Task { @MainActor in
                guard let self, self.isActive, self.generation == myGeneration else { return }
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
