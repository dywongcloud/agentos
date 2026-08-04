import Combine
import Foundation

#if canImport(CryptoKit)
import CryptoKit
#endif

#if canImport(HoloirohIosBridge)
import HoloirohIosBridge
#endif

/// Owns one bridge handle for a connected session.
///
/// - The media stream uses `IrohLiveFrameSource`.
/// - The control channel authenticates with a personal identification number (PIN).
/// - The control channel uses `FFIControlChannelSender`.
/// - `ffiQueue` serializes control-channel bridge calls.
/// - `shutdown()` stops the media subscription before it frees the bridge.
///
/// Bridge-less builds report a failed connection and keep the app fallbacks available.
final class HoloConnection: ObservableObject {

    /// Defines the published connection lifecycle.
    enum Phase: Equatable {
        /// Indicates that no connection is active. `shutdown()` also returns the phase to this value.
        case idle
        /// Indicates that bridge creation, ticket connection, or PIN authentication is in progress.
        case connecting
        /// Indicates that the media stream and control channel are available.
        case connected
        /// Indicates a connection failure. The associated string describes the cause.
        case failed(String)
    }

    @Published private(set) var phase: Phase = .idle

    /// Provides the media-stream source after `phase` becomes `.connected`.
    /// The source and phase change during the same main-thread turn.
    private(set) var liveFrameSource: VideoFrameSource?

    /// Provides the outbound control channel after connection. Callers use a logging fallback while this value is `nil`.
    private(set) var controlSender: ControlChannelSending?

    /// Receives decoded daemon messages and send results on the main thread.
    /// Assign this closure before you call `connect`.
    var onServerMessage: ((ServerMessage) -> Void)?

    /// Serializes all control-channel Foreign Function Interface (FFI) calls on the bridge.
    private let ffiQueue = DispatchQueue(label: "com.holoiroh.connection.control")

    /// Prevents state publication after permanent shutdown.
    /// Late `ffiQueue` completions check this main-thread value.
    private var isShutdown = false

    private let secretKey: Data?
    private let identityErrorDescription: String?

    init(identityStore: IrohIdentityStore = IrohIdentityStore()) {
        do {
            self.secretKey = try identityStore.loadOrCreateSeed()
            self.identityErrorDescription = nil
        } catch {
            self.secretKey = nil
            self.identityErrorDescription = error.localizedDescription
        }
    }

    #if canImport(HoloirohIosBridge)

    /// Protects `_bridge` across `ffiQueue`, the main thread, and `deinit`.
    /// All ownership claims use `takeBridge()`.
    private let bridgeLock = NSLock()
    private var _bridge: OpaquePointer?

    private let generationLock = NSLock()
    private var connectionGeneration: UInt64 = 0
    private let endpointLeaseLock = NSLock()
    private var endpointLeaseHeld = false

    private func acquireEndpointLease() {
        IrohEndpointCoordinator.shared.acquire()
        endpointLeaseLock.lock()
        endpointLeaseHeld = true
        endpointLeaseLock.unlock()
    }

    private func takeEndpointLease() -> Bool {
        endpointLeaseLock.lock()
        defer { endpointLeaseLock.unlock() }
        let held = endpointLeaseHeld
        endpointLeaseHeld = false
        return held
    }

    private func releaseEndpointLease() {
        if takeEndpointLease() {
            IrohEndpointCoordinator.shared.release()
        }
    }

    private func advanceConnectionGeneration() -> UInt64 {
        generationLock.lock()
        defer { generationLock.unlock() }
        connectionGeneration &+= 1
        return connectionGeneration
    }

    private func isCurrentConnectionGeneration(_ generation: UInt64) -> Bool {
        generationLock.lock()
        defer { generationLock.unlock() }
        return connectionGeneration == generation
    }

    private var bridge: OpaquePointer? {
        get { bridgeLock.lock(); defer { bridgeLock.unlock() }; return _bridge }
        set { bridgeLock.lock(); defer { bridgeLock.unlock() }; _bridge = newValue }
    }

    /// Claims the bridge pointer once. This prevents a double free during concurrent shutdown and connection failure.
    private func takeBridge() -> OpaquePointer? {
        bridgeLock.lock(); defer { bridgeLock.unlock() }
        let claimed = _bridge
        _bridge = nil
        return claimed
    }

    /// Drains `poll_control_event` repeatedly on `ffiQueue`.
    /// The timer lets `control_send` calls run between drain operations.
    private var eventPump: DispatchSourceTimer?

    /// Tracks the verified session identifier and outbound sequence.
    /// The bridge verifies daemon envelopes and signs each outbound Swift envelope.
    private var sessionState = OutboundEnvelopeState()

    #endif

    // MARK: - Lifecycle

    /// Creates the bridge, connects the ticket, and authenticates the control channel with the PIN.
    /// Work runs off the main thread.
    /// Phase updates run on the main thread.
    /// Repeated calls do nothing after the idle phase.
    func connect(ticket: String, pin: String) {
        guard !isShutdown, phase == .idle else { return }
        phase = .connecting
        #if canImport(HoloirohIosBridge)
        let generation = advanceConnectionGeneration()
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            guard let self else { return }
            self.acquireEndpointLease()
            self.ffiQueue.async {
                guard !self.isShutdown,
                      self.isCurrentConnectionGeneration(generation)
                else {
                    self.releaseEndpointLease()
                    return
                }
                self.establish(ticket: ticket, pin: pin, generation: generation)
            }
        }
        #else
        phase = .failed(
            "HoloirohIosBridge not linked (simulator/CI build) -- demo transport only"
        )
        #endif
    }

    /// Resets the session to `.idle` and permits a later connection.
    /// Stops the media source before it frees the bridge.
    /// Does nothing while a connection attempt is in progress.
    func reset() {
        guard !isShutdown, phase != .connecting else { return }
        controlSender = nil
        let source = liveFrameSource
        liveFrameSource = nil
        phase = .idle
        #if canImport(HoloirohIosBridge)
        _ = advanceConnectionGeneration()
        eventPump?.cancel()
        eventPump = nil
        freeBridgeAfterStopping(source)
        #else
        source?.stop()
        #endif
    }

    /// Permanently closes the session and returns the phase to `.idle`.
    /// Later connection attempts do nothing.
    /// Repeated calls are safe.
    func shutdown() {
        guard !isShutdown else { return }
        controlSender = nil
        let source = liveFrameSource
        liveFrameSource = nil
        phase = .idle
        isShutdown = true
        #if canImport(HoloirohIosBridge)
        _ = advanceConnectionGeneration()
        eventPump?.cancel()
        eventPump = nil
        freeBridgeAfterStopping(source)
        #else
        source?.stop()
        #endif
    }

    deinit {
        // Safety net if `shutdown()` was never called. Safe here: all
        // FFI-queue work holds `self` weakly, so nothing else can be
        // mutating the handle once deinit runs.
        #if canImport(HoloirohIosBridge)
        eventPump?.cancel()
        if !isShutdown {
            freeBridgeAfterStopping(liveFrameSource)
        }
        #endif
    }

    // MARK: - FFI-backed implementation

    #if canImport(HoloirohIosBridge)

    /// Creates and authenticates the bridge on `ffiQueue` for one connection generation.
    private func establish(ticket: String, pin: String, generation: UInt64) {
        // Fresh per connection attempt: a reconnect gets a new daemon-minted
        // `session_id` and its own sequence numbering from zero (see
        // `sessionState`'s doc).
        sessionState = OutboundEnvelopeState()

        guard let secretKey else {
            releaseEndpointLease()
            reportFailure(
                "Iroh identity unavailable: \(identityErrorDescription ?? "unknown Keychain error")",
                generation: generation
            )
            return
        }
        let created = secretKey.withUnsafeBytes { keyBytes in
            holoiroh_ios_bridge_new_with_secret_key(
                keyBytes.bindMemory(to: UInt8.self).baseAddress,
                UInt(keyBytes.count)
            )
        }
        guard let created else {
            releaseEndpointLease()
            reportFailure("holoiroh_ios_bridge_new_with_secret_key returned null", generation: generation)
            return
        }
        bridge = created

        var err: UnsafeMutablePointer<CChar>?
        let ticketStatus = ticket.withCString { cstr in
            holoiroh_ios_bridge_ticket_connect(created, cstr, &err)
        }
        guard ticketStatus == HOLOIROH_OK else {
            if let claimed = takeBridge() {
                holoiroh_ios_bridge_free(claimed)
            }
            releaseEndpointLease()
            reportFailure(
                describeFFIFailure("ticket_connect", status: ticketStatus, err: &err),
                generation: generation
            )
            return
        }

        let controlStatus = pin.withCString { cstr in
            holoiroh_ios_bridge_control_connect(created, cstr, &err)
        }
        guard controlStatus == HOLOIROH_OK else {
            if let claimed = takeBridge() {
                holoiroh_ios_bridge_free(claimed)
            }
            releaseEndpointLease()
            reportFailure(
                describeFFIFailure("control_connect", status: controlStatus, err: &err),
                generation: generation
            )
            return
        }

        // Both planes share this one bridge from here on: the frame source
        // subscribes + polls video on its own queue; control send/poll stay
        // on `ffiQueue`.
        let source = IrohLiveFrameSource(bridge: created)
        let sender = FFIControlChannelSender(
            bridge: created,
            queue: ffiQueue,
            sessionState: sessionState,
            report: { [weak self] message, wire in
                guard let self, self.isCurrentConnectionGeneration(generation) else { return }
                // Runs on main (the sender hops there): surface the confirmed
                // wire send in the same log stream as daemon events.
                //
                // Remote-control events are deliberately NOT logged. They are
                // emitted at touch-tracking frequency (~60Hz for the whole
                // duration of a drag), and every log entry appends to MainView's
                // @State log array -- which re-evaluates that entire ~2200-line
                // body. Logging them turned the product's most latency-sensitive
                // interaction, dragging to control the Mac, into the moment the
                // UI does the most work per second, and grew an unbounded array
                // of envelopes nobody reads. Their effect is already visible to
                // the user directly: the Mac's cursor moves.
                if case .remoteControl = message { return }
                let summary = safeOutboundWireSummary(message: message, wire: wire)
                self.onServerMessage?(.status(text: summary))
            },
            reportError: { [weak self] detail in
                guard let self, self.isCurrentConnectionGeneration(generation) else { return }
                self.onServerMessage?(.error(text: detail))
            }
        )

        DispatchQueue.main.async { [weak self] in
            guard let self,
                  !self.isShutdown,
                  self.isCurrentConnectionGeneration(generation)
            else { return }
            // instant a `phase` observer fires.
            self.liveFrameSource = source
            self.controlSender = sender
            self.startEventPump(generation: generation)
            self.phase = .connected
        }
    }

    /// Starts a timer on `ffiQueue` that drains pending control events.
    /// Delivers decoded messages on the main thread.
    private func startEventPump(generation: UInt64) {
        let timer = DispatchSource.makeTimerSource(queue: ffiQueue)
        timer.schedule(deadline: .now(), repeating: .milliseconds(150), leeway: .milliseconds(50))
        timer.setEventHandler { [weak self] in
            self?.drainControlEvents(generation: generation)
        }
        timer.resume()
        eventPump = timer
    }

    /// Drains pending control events on `ffiQueue`.
    /// Stops when the bridge reports no event or returns an error.
    private func drainControlEvents(generation: UInt64) {
        guard isCurrentConnectionGeneration(generation), let bridge = bridge else { return }
        while true {
            var outJSON: UnsafeMutablePointer<CChar>?
            var outErr: UnsafeMutablePointer<CChar>?
            let status = holoiroh_ios_bridge_poll_control_event(bridge, &outJSON, &outErr)
            guard status == HOLOIROH_OK else {
                var detail = "poll_control_event failed (\(status))"
                if let e = outErr {
                    detail += ": " + String(cString: e)
                    holoiroh_ios_bridge_free_error_string(e)
                }
                // Surface a broken control stream once, not every tick.
                DispatchQueue.main.async { [weak self] in
                    guard let self,
                          !self.isShutdown,
                          self.isCurrentConnectionGeneration(generation)
                    else { return }
                    self.eventPump?.cancel()
                    self.eventPump = nil
                    self.onServerMessage?(.error(text: detail))
                    // The pump is the ONLY thing reading the control channel, so
                    // cancelling it means this connection can never again receive
                    // anything from the daemon. Previously the failure stopped
                    // here: `phase` stayed `.connected`, so MainView's
                    // reconnect-on-failure never fired and the app sat claiming
                    // it was connected, with a permanently dead control channel,
                    // until the user force-quit. Publishing `.failed` is what
                    // actually hands the problem to the reconnect path.
                    self.phase = .failed(detail)
                }
                return
            }
            guard let json = outJSON else {
                return // drained -- no event pending
            }
            let line = String(cString: json)
            holoiroh_ios_bridge_free_error_string(json)
            deliver(decodeServerLine(line), generation: generation)
        }
    }

    /// Decodes the envelope shape used after the PIN handshake.
    /// Each envelope contains the daemon session identifier and a `ServerMessage` payload.
    private struct EnvelopedServerMessage: Decodable {
        let sessionId: String
        let payload: ServerMessage

        private enum CodingKeys: String, CodingKey {
            case sessionId = "session_id"
            case payload
        }
    }

    /// Decodes one verified server envelope.
    /// Captures its session identifier for outbound messages.
    /// Reports bounded metadata for unrecognized data.
    private func decodeServerLine(_ line: String) -> ServerMessage {
        let data = Data(line.utf8)
        let decoder = JSONDecoder()
        if let enveloped = try? decoder.decode(EnvelopedServerMessage.self, from: data) {
            sessionState.sessionId = enveloped.sessionId
            return enveloped.payload
        }
        return .status(text: safeInboundWireSummary(data))
    }

    /// Delivers one server message on the main thread for the current connection generation.
    private func deliver(_ message: ServerMessage, generation: UInt64) {
        DispatchQueue.main.async { [weak self] in
            guard let self,
                  !self.isShutdown,
                  self.isCurrentConnectionGeneration(generation)
            else { return }
            self.onServerMessage?(message)
        }
    }

    /// Formats an FFI failure and frees the returned error string.
    private func describeFFIFailure(
        _ what: String,
        status: Int32,
        err: inout UnsafeMutablePointer<CChar>?
    ) -> String {
        if let e = err {
            let detail = String(cString: e)
            holoiroh_ios_bridge_free_error_string(e)
            err = nil
            return "\(what) failed (\(status)): \(detail)"
        }
        return "\(what) failed (\(status))"
    }

    /// Publishes `.failed` on the main thread for the current connection generation.
    private func reportFailure(_ detail: String, generation: UInt64) {
        DispatchQueue.main.async { [weak self] in
            guard let self,
                  !self.isShutdown,
                  self.isCurrentConnectionGeneration(generation)
            else { return }
            self.phase = .failed(detail)
        }
    }

    /// Stops the media source before it frees the bridge on `ffiQueue`.
    /// This order keeps the subscription decoder on a valid bridge runtime.
    /// The method also releases the endpoint lease.
    private func freeBridgeAfterStopping(_ source: VideoFrameSource?) {
        let queue = ffiQueue
        let bridgeToFree = takeBridge()
        let releasesEndpointLease = takeEndpointLease()
        let freeBridge: () -> Void = {
            queue.async {
                if let bridgeToFree {
                    holoiroh_ios_bridge_free(bridgeToFree)
                }
                if releasesEndpointLease {
                    IrohEndpointCoordinator.shared.release()
                }
            }
        }
        if let live = source as? IrohLiveFrameSource {
            live.stop(completion: freeBridge)
        } else {
            source?.stop()
            freeBridge()
        }
    }

    #endif
}

#if canImport(HoloirohIosBridge)

private func safeInboundWireSummary(_ data: Data) -> String {
    #if canImport(CryptoKit)
    let digest = SHA256.hash(data: data).prefix(8).map { String(format: "%02x", $0) }.joined()
    #else
    let digest = "unavailable"
    #endif
    return "unrecognized control event bytes=\(data.count) sha256=\(digest)"
}

private struct SafeOutboundEnvelopeMetadata: Decodable {
    let messageId: String

    private enum CodingKeys: String, CodingKey {
        case messageId = "message_id"
    }
}

private func safeOutboundWireSummary(message: ClientMessage, wire: String) -> String {
    let data = Data(wire.utf8)
    let decodedId = try? JSONDecoder().decode(SafeOutboundEnvelopeMetadata.self, from: data).messageId
    let messageId = decodedId.map { String($0.prefix(64)) } ?? "unknown"
    #if canImport(CryptoKit)
    let digest = SHA256.hash(data: data).prefix(8).map { String(format: "%02x", $0) }.joined()
    #else
    let digest = "unavailable"
    #endif
    return "→ sent kind=\(message.wireKindLabel) message_id=\(messageId) bytes=\(data.count) sha256=\(digest)"
}

/// Sends typed control-channel messages through the Rust bridge.
///
/// - The bridge accepts unsigned typed envelopes only.
/// - The bridge validates the session and message type.
/// - The bridge signs envelopes with its endpoint identity.
/// - The serial FFI queue owns bridge access.
/// - Result closures run on the main thread.
///
/// The sender is valid only while its `HoloConnection` is connected.
final class FFIControlChannelSender: ControlChannelSending {
    private let bridge: OpaquePointer
    private let queue: DispatchQueue
    /// Shares the session identifier and sequence counter with the owning connection.
    private let sessionState: OutboundEnvelopeState
    /// Reports a successful send on the main thread.
    private let report: (ClientMessage, String) -> Void
    /// Reports a send failure on the main thread.
    private let reportError: (String) -> Void

    init(
        bridge: OpaquePointer,
        queue: DispatchQueue,
        sessionState: OutboundEnvelopeState,
        report: @escaping (ClientMessage, String) -> Void,
        reportError: @escaping (String) -> Void
    ) {
        self.bridge = bridge
        self.queue = queue
        self.sessionState = sessionState
        self.report = report
        self.reportError = reportError
    }

    /// Stores the newest cursor move until the FFI queue can write it. New moves replace older staged moves.
    private let moves = MoveCoalescer<ClientMessage>()

    func send(_ message: ClientMessage) {
        guard case .remoteControl(.move(_, _)) = message else {
            sendWithRetry(message, retriesLeft: 20)
            return
        }
        coalesceMove(message)
    }

    /// Sends the newest staged cursor move.
    /// Each move schedules one flush on the serial queue.
    /// A slow link can coalesce moves that it cannot transmit in time.
    /// This path does not retry moves because delayed cursor positions are stale.
    private func coalesceMove(_ message: ClientMessage) {
        moves.stage(message)
        queue.async {
            guard let move = self.moves.takeLatest() else { return }
            self.writeToBridge(move, reportSuccess: false)
        }
    }

    /// Retries a send while the daemon session identifier is unavailable.
    /// The event pump can receive the identifier after the connection phase changes.
    /// Retries occur every 100 milliseconds for up to 2 seconds.
    /// After 2 seconds, the sender reports an error.
    private func sendWithRetry(_ message: ClientMessage, retriesLeft: Int) {
        let reportError = reportError
        // Encoding happens on `queue` too (not the caller's thread): the
        // greeting's `session_id` is written from this same serial queue in
        // `HoloConnection.decodeServerLine`, so reading it here as well keeps
        // the "has the greeting arrived yet" check and the encode itself on
        // one queue instead of racing a caller thread against it.
        queue.async {
            guard self.writeToBridge(message, reportSuccess: true) == .noSessionYet else { return }
            if retriesLeft > 0 {
                self.queue.asyncAfter(deadline: .now() + 0.1) {
                    self.sendWithRetry(message, retriesLeft: retriesLeft - 1)
                }
            } else {
                DispatchQueue.main.async {
                    reportError("control send \(message.wireKindLabel) failed: no session_id yet (daemon greeting not received)")
                }
            }
        }
    }

    enum BridgeWriteOutcome {
        case sent
        case refused
        /// Indicates that the daemon session identifier is not available yet.
        case noSessionYet
    }

    /// Encodes one message and writes it to the bridge. Call this method only on `queue`.
    @discardableResult
    private func writeToBridge(_ message: ClientMessage, reportSuccess: Bool) -> BridgeWriteOutcome {
        guard let wire = encoded(message, sessionState: sessionState) else { return .noSessionYet }
        var err: UnsafeMutablePointer<CChar>?
        let status = wire.withCString { cstr in
            holoiroh_ios_bridge_control_send(bridge, cstr, &err)
        }
        if status == HOLOIROH_OK {
            if reportSuccess {
                let report = report
                DispatchQueue.main.async { report(message, wire) }
            }
            return .sent
        }
        var detail = "control send \(message.wireKindLabel) failed (\(status))"
        if let e = err {
            detail += ": " + String(cString: e)
            holoiroh_ios_bridge_free_error_string(e)
        }
        let reportError = reportError
        DispatchQueue.main.async { reportError(detail) }
        return .refused
    }
}

#endif
