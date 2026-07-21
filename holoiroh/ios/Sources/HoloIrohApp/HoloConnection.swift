import Combine
import Foundation

#if canImport(HoloirohIosBridge)
import HoloirohIosBridge
#endif

/// Owns the ONE `holoiroh-ios-bridge` handle a connected session runs on, and
/// both planes that share it:
///
/// - **Video plane**: after `ticket_connect`, the bridge is handed to a
///   shared-bridge `IrohLiveFrameSource` (`init(bridge:)`), which subscribes
///   to the `iroh-live` video track and polls frames on its own serial queue.
/// - **Control plane**: `control_connect(pin)` performs the PROTOCOL.md
///   pre-session PIN handshake on the control-ALPN stream. Outbound
///   `ClientMessage`s then go through `FFIControlChannelSender` (below), and
///   inbound `ServerMessage` NDJSON lines are drained by a repeating pump on
///   the same serial FFI queue and delivered -- decoded -- to
///   `onServerMessage` on the main thread.
///
/// ## Threading / ownership
/// Every control-plane FFI call (`new`, `ticket_connect`, `control_connect`,
/// `control_send`, `poll_control_event`, `free`) happens on the private
/// serial `ffiQueue`, so the bridge's control plane has a single owning
/// thread. The frame source owns the *subscription* handle on its own queue
/// (`poll_next_frame`/`subscription_free` take the subscription, not the
/// bridge); the only bridge call it makes is the one-shot `subscribe` at
/// start, which the bridge (a Tokio-runtime wrapper) must tolerate alongside
/// control traffic -- that concurrency is part of the shared-bridge FFI
/// contract this type codes against.
///
/// Teardown order is enforced: `shutdown()` stops the frame source first and
/// frees the bridge only from that stop's completion (subscription before
/// bridge -- the subscription's decoder is driven by the bridge's runtime).
///
/// ## Bridge-less builds
/// Under `#if !canImport(HoloirohIosBridge)` (the headless simulator/CI
/// `swift build`), `connect` immediately reports `.failed` with an
/// explanatory reason and the UI keeps its synthetic-video +
/// logging-sender fallbacks -- the same compile-honest stub pattern as
/// `IrohLiveFrameSource`.
final class HoloConnection: ObservableObject {

    /// Coarse connection lifecycle, published so the dashboard can react.
    enum Phase: Equatable {
        /// Not yet connected (initial state, and after `shutdown`).
        case idle
        /// `connect` is running (bridge create / ticket connect / PIN handshake).
        case connecting
        /// Both planes are up: live frames + real control channel available.
        case connected
        /// The connection could not be established (or the bridge is not
        /// linked in this build). The associated value is the reason.
        case failed(String)
    }

    @Published private(set) var phase: Phase = .idle

    /// The shared-bridge live video source. Non-nil from the moment `phase`
    /// becomes `.connected` -- both are set in the same main-thread turn, so
    /// a `phase` observer can read this immediately.
    private(set) var liveFrameSource: VideoFrameSource?

    /// The real outbound control channel, non-nil once `.connected`. `nil`
    /// before then (callers fall back to the logging stand-in).
    private(set) var controlSender: ControlChannelSending?

    /// Decoded daemon events (and wire-level send confirmations/errors in the
    /// same `ServerMessage` shape), delivered on the main thread. Assign
    /// before calling `connect`.
    var onServerMessage: ((ServerMessage) -> Void)?

    /// Serial queue owning every control-plane FFI call on the bridge.
    private let ffiQueue = DispatchQueue(label: "com.holoiroh.connection.control")

    /// Set once `shutdown()` has run (main thread only). Late completions
    /// hopping back from `ffiQueue` check it so a torn-down connection never
    /// resurrects published state.
    private var isShutdown = false

    #if canImport(HoloirohIosBridge)

    /// Guards `_bridge`: it is written on `ffiQueue` (establish) and claimed
    /// on the main thread (`shutdown`) or in `deinit`, so access is locked
    /// and freeing goes through the claim-once `takeBridge()`.
    private let bridgeLock = NSLock()
    private var _bridge: OpaquePointer?

    private var bridge: OpaquePointer? {
        get { bridgeLock.lock(); defer { bridgeLock.unlock() }; return _bridge }
        set { bridgeLock.lock(); defer { bridgeLock.unlock() }; _bridge = newValue }
    }

    /// Atomically takes ownership of the bridge pointer (exactly one caller
    /// can win), so shutdown racing a failed establish can never double-free.
    private func takeBridge() -> OpaquePointer? {
        bridgeLock.lock(); defer { bridgeLock.unlock() }
        let claimed = _bridge
        _bridge = nil
        return claimed
    }

    /// Repeating drain of `poll_control_event`, on `ffiQueue`. Timer-driven
    /// rather than a tight loop so `control_send` calls interleave on the
    /// same serial queue.
    private var eventPump: DispatchSourceTimer?

    /// This connection's outbound envelope state (`session_id` + sequence
    /// counter -- see `OutboundEnvelopeState`'s doc). `session_id` starts
    /// `nil` and is populated the moment the daemon's envelope-wrapped
    /// greeting is observed in `decodeServerLine`; `FFIControlChannelSender`
    /// shares this exact instance, so every outbound send after the greeting
    /// carries the daemon-assigned `session_id`. Fresh per `establish()` call
    /// (a reconnect gets a new daemon-minted session, so the old id/sequence
    /// must not carry over).
    private var sessionState = OutboundEnvelopeState()

    #endif

    // MARK: - Lifecycle

    /// Opens the real connection: bridge create → `ticket_connect(ticket)`
    /// (video plane) → `control_connect(pin)` (control-ALPN PIN handshake).
    /// Runs off-main; publishes `.connected`/`.failed` on main. Idempotent
    /// once past `.idle`.
    func connect(ticket: String, pin: String) {
        guard !isShutdown, phase == .idle else { return }
        phase = .connecting
        #if canImport(HoloirohIosBridge)
        ffiQueue.async { [weak self] in
            self?.establish(ticket: ticket, pin: pin)
        }
        #else
        phase = .failed(
            "HoloirohIosBridge not linked (simulator/CI build) -- demo transport only"
        )
        #endif
    }

    /// Tears the current session down but leaves this object REUSABLE: back
    /// to `.idle`, ready for another `connect(ticket:pin:)`. This is the
    /// auto-reconnect primitive (see `MainView`'s foreground/failure
    /// recovery): when the QUIC session dies while the app is backgrounded
    /// (iOS suspends the process; the daemon side times the connection out),
    /// the ONLY way back to live video is a fresh bridge + ticket connect +
    /// PIN handshake -- restarting the frame source alone re-subscribes on a
    /// dead bridge, which is exactly the live-witnessed "black screen and
    /// errors out after switching apps" bug.
    ///
    /// Refused while `.connecting`: `establish` is mid-flight on `ffiQueue`
    /// and its main-thread completions only check `isShutdown` -- resetting
    /// under it could interleave a stale publication with the new session's.
    /// Every real call site (failure recovery, foreground recovery) runs
    /// from `.failed`/`.connected`, where `establish` has already finished.
    func reset() {
        guard !isShutdown, phase != .connecting else { return }
        controlSender = nil
        let source = liveFrameSource
        liveFrameSource = nil
        phase = .idle
        #if canImport(HoloirohIosBridge)
        eventPump?.cancel()
        eventPump = nil
        freeBridgeAfterStopping(source)
        #else
        source?.stop()
        #endif
    }

    /// Tears the session down PERMANENTLY: like `reset()`, but this object
    /// refuses all further use (`isShutdown`). Idempotent; call on
    /// Disconnect / screen teardown.
    func shutdown() {
        guard !isShutdown else { return }
        controlSender = nil
        let source = liveFrameSource
        liveFrameSource = nil
        phase = .idle
        isShutdown = true
        #if canImport(HoloirohIosBridge)
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

    /// Runs on `ffiQueue`. Creates the one bridge, ticket-connects it (video
    /// plane), then performs the control-ALPN PIN handshake (control plane).
    private func establish(ticket: String, pin: String) {
        // Fresh per connection attempt: a reconnect gets a new daemon-minted
        // `session_id` and its own sequence numbering from zero (see
        // `sessionState`'s doc).
        sessionState = OutboundEnvelopeState()

        guard let created = holoiroh_ios_bridge_new() else {
            reportFailure("holoiroh_ios_bridge_new returned null")
            return
        }
        bridge = created

        var err: UnsafeMutablePointer<CChar>?
        let ticketStatus = ticket.withCString { cstr in
            holoiroh_ios_bridge_ticket_connect(created, cstr, &err)
        }
        guard ticketStatus == HOLOIROH_OK else {
            if let claimed = takeBridge() { holoiroh_ios_bridge_free(claimed) }
            reportFailure(describeFFIFailure("ticket_connect", status: ticketStatus, err: &err))
            return
        }

        let controlStatus = pin.withCString { cstr in
            holoiroh_ios_bridge_control_connect(created, cstr, &err)
        }
        guard controlStatus == HOLOIROH_OK else {
            if let claimed = takeBridge() { holoiroh_ios_bridge_free(claimed) }
            reportFailure(describeFFIFailure("control_connect", status: controlStatus, err: &err))
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
                // Runs on main (the sender hops there): surface the confirmed
                // wire send in the same log stream as daemon events.
                let trimmed = wire.trimmingCharacters(in: .whitespacesAndNewlines)
                self?.onServerMessage?(.status(text: "→ sent \(message.wireKindLabel): \(trimmed)"))
            },
            reportError: { [weak self] detail in
                self?.onServerMessage?(.error(text: detail))
            }
        )

        DispatchQueue.main.async { [weak self] in
            guard let self, !self.isShutdown else { return }
            // Order matters: the frame source and sender must be readable the
            // instant a `phase` observer fires.
            self.liveFrameSource = source
            self.controlSender = sender
            self.startEventPump()
            self.phase = .connected
        }
    }

    /// Starts the control-event pump: a repeating timer on `ffiQueue` that
    /// drains every pending `poll_control_event` line and delivers the
    /// decoded `ServerMessage`s on the main thread.
    private func startEventPump() {
        let timer = DispatchSource.makeTimerSource(queue: ffiQueue)
        timer.schedule(deadline: .now(), repeating: .milliseconds(150), leeway: .milliseconds(50))
        timer.setEventHandler { [weak self] in
            self?.drainControlEvents()
        }
        timer.resume()
        eventPump = timer
    }

    /// Runs on `ffiQueue`. Drains all pending control events (the daemon can
    /// emit bursts), stopping when the bridge reports none pending.
    private func drainControlEvents() {
        guard let bridge = bridge else { return }
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
                    guard let self, !self.isShutdown else { return }
                    self.eventPump?.cancel()
                    self.eventPump = nil
                    self.onServerMessage?(.error(text: detail))
                }
                return
            }
            guard let json = outJSON else {
                return // drained -- no event pending
            }
            let line = String(cString: json)
            holoiroh_ios_bridge_free_error_string(json)
            deliver(decodeServerLine(line))
        }
    }

    /// A `TaskEnvelope`d server line: `{ "session_id": ..., "payload": { "type": ... }, ... }`.
    /// The bridge is expected to hand up bare `ServerMessage` lines only for
    /// the pre-session PIN handshake reply; every message from the greeting
    /// onward is envelope-wrapped and carries the daemon-minted `session_id`
    /// this connection must echo on every outbound send (see
    /// `OutboundEnvelopeState`).
    private struct EnvelopedServerMessage: Decodable {
        let sessionId: String
        let payload: ServerMessage

        private enum CodingKeys: String, CodingKey {
            case sessionId = "session_id"
            case payload
        }
    }

    /// Decodes one NDJSON line from `poll_control_event` into a
    /// `ServerMessage` (directly, or via a `TaskEnvelope` payload -- in which
    /// case this connection's `sessionId` is captured/refreshed from it, so
    /// outbound sends can be envelope-wrapped correctly; see
    /// `ControlChannelSender.swift`'s `OutboundEnvelope`). Undecodable lines
    /// are surfaced as status entries instead of vanishing.
    private func decodeServerLine(_ line: String) -> ServerMessage {
        let data = Data(line.utf8)
        let decoder = JSONDecoder()
        if let enveloped = try? decoder.decode(EnvelopedServerMessage.self, from: data) {
            sessionState.sessionId = enveloped.sessionId
            return enveloped.payload
        }
        if let direct = try? decoder.decode(ServerMessage.self, from: data) {
            return direct
        }
        let trimmed = line.trimmingCharacters(in: .whitespacesAndNewlines)
        return .status(text: "unrecognized control event: \(trimmed)")
    }

    /// Hands one message to `onServerMessage` on the main thread.
    private func deliver(_ message: ServerMessage) {
        DispatchQueue.main.async { [weak self] in
            guard let self, !self.isShutdown else { return }
            self.onServerMessage?(message)
        }
    }

    /// Formats an FFI failure, consuming (and freeing) the out_error string.
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

    /// Publishes `.failed` on the main thread (unless already shut down).
    private func reportFailure(_ detail: String) {
        DispatchQueue.main.async { [weak self] in
            guard let self, !self.isShutdown else { return }
            self.phase = .failed(detail)
        }
    }

    /// Stops `source` (a shared-bridge frame source frees its subscription on
    /// its own queue first) and then frees the bridge on `ffiQueue` -- free
    /// order is subscription first, bridge second, because the subscription's
    /// decoder is driven by the bridge's runtime. Captures the claimed bridge
    /// pointer by value so this also works from `deinit`.
    private func freeBridgeAfterStopping(_ source: VideoFrameSource?) {
        let queue = ffiQueue
        let bridgeToFree = takeBridge()
        let freeBridge: () -> Void = {
            guard let bridgeToFree else { return }
            queue.async {
                holoiroh_ios_bridge_free(bridgeToFree)
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

/// The real `ControlChannelSending`: writes each `ClientMessage`'s NDJSON
/// wire line (the shared `encoded(_:sessionState:)` helper -- byte-identical to what the
/// `LoggingControlChannelSender` stand-in produced) to the daemon over the
/// bridge's control-ALPN stream via `holoiroh_ios_bridge_control_send`.
/// Sends hop to the owning `HoloConnection`'s serial FFI queue so the
/// bridge's control plane is only ever touched from one thread; results are
/// reported back on the main thread through the two closures. Valid only
/// while the owning `HoloConnection` is connected (it is discarded on
/// `shutdown`, and pre-shutdown sends drain on the serial queue before the
/// bridge is freed).
final class FFIControlChannelSender: ControlChannelSending {
    private let bridge: OpaquePointer
    private let queue: DispatchQueue
    /// This connection's shared outbound envelope state (`session_id` +
    /// sequence counter) -- the same instance `HoloConnection` populates from
    /// the daemon's greeting in `decodeServerLine`. See `OutboundEnvelopeState`.
    private let sessionState: OutboundEnvelopeState
    /// Called on the main thread after a successful send (message + wire line).
    private let report: (ClientMessage, String) -> Void
    /// Called on the main thread when a send fails (human-readable detail).
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

    func send(_ message: ClientMessage) {
        sendWithRetry(message, retriesLeft: 20)
    }

    /// The greeting that carries the daemon-minted `session_id` races any
    /// send fired the instant the connection reports `.connected` (the event
    /// pump only polls every 150ms) -- live-witnessed: the auto-pair prompt
    /// silently dropped with "no session_id yet" because the one send
    /// attempt happened a few ms before the greeting was decoded. A send
    /// that arrives before the greeting therefore RETRIES briefly (100ms *
    /// 20 = up to 2s) instead of dropping; a genuinely missing greeting
    /// still surfaces the error after the window.
    private func sendWithRetry(_ message: ClientMessage, retriesLeft: Int) {
        let bridge = bridge
        let sessionState = sessionState
        let report = report
        let reportError = reportError
        // Encoding happens on `queue` too (not the caller's thread): the
        // greeting's `session_id` is written from this same serial queue in
        // `HoloConnection.decodeServerLine`, so reading it here as well keeps
        // the "has the greeting arrived yet" check and the encode itself on
        // one queue instead of racing a caller thread against it.
        queue.async {
            guard let wire = self.encoded(message, sessionState: sessionState) else {
                if retriesLeft > 0 {
                    self.queue.asyncAfter(deadline: .now() + 0.1) {
                        self.sendWithRetry(message, retriesLeft: retriesLeft - 1)
                    }
                } else {
                    DispatchQueue.main.async {
                        reportError("control send \(message.wireKindLabel) failed: no session_id yet (daemon greeting not received)")
                    }
                }
                return
            }
            var err: UnsafeMutablePointer<CChar>?
            let status = wire.withCString { cstr in
                holoiroh_ios_bridge_control_send(bridge, cstr, &err)
            }
            if status == HOLOIROH_OK {
                DispatchQueue.main.async { report(message, wire) }
            } else {
                var detail = "control send \(message.wireKindLabel) failed (\(status))"
                if let e = err {
                    detail += ": " + String(cString: e)
                    holoiroh_ios_bridge_free_error_string(e)
                }
                DispatchQueue.main.async { reportError(detail) }
            }
        }
    }
}

#endif
