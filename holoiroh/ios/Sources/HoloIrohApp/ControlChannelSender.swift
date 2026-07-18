import Foundation

/// The seam through which the app sends a `ClientMessage` (`Models/ServerMessage.swift`)
/// to the Mac daemon over the control channel.
///
/// This is the single injection point the UI talks to when it needs to *send*
/// something -- most importantly the remote **kill-switch** `ClientMessage.stop`
/// wired to the Working/Connecting "Cancel" control (see `MainView.sessionActions`).
/// Keeping it a protocol means the view layer never references the transport
/// directly: today it is handed the `LoggingControlChannelSender` stand-in, and
/// the day the real `iroh` transport exists it is handed a different conforming
/// type at exactly one place (`MainView`'s initializer default) with no change to
/// any button, panel, or action closure.
///
/// ## The one remaining wiring step to the real transport
///
/// `send(_:)` is already given the correct, protocol-shaped `ClientMessage` value
/// (`.stop` for the kill switch, `.prompt`/`.voiceTranscript` for instructions).
/// The **only** thing not yet real is the byte path out of the device: there is
/// no `iroh`/FFI networking in this skeleton (see `holoiroh/README.md` and
/// `ios/IROH_FFI.md`). The remaining step is a single new type conforming to this
/// protocol whose `send(_:)` calls the `ios-bridge` control-channel send
/// (`holoiroh/ios-bridge/src/lib.rs`'s `extern "C"` surface -- the hand-written
/// Rust staticlib that wraps `iroh`'s `Connection::open_bi` and writes one
/// newline-delimited JSON line per `PROTOCOL.md`). Concretely, that type would:
///
///   1. hold the opaque connection handle returned by the bridge's
///      ticket-connect call (the same handle the live-video subscription uses),
///   2. in `send(_:)`, JSON-encode the `ClientMessage` (the `encoded(_:)` helper
///      below already produces the exact `PROTOCOL.md` NDJSON bytes -- e.g.
///      `{"type":"stop"}\n`), optionally wrap it in the `TaskEnvelope` shape once
///      the client tracks a `session_id`/`sequence_number`, and hand those bytes
///      to the bridge's `send` FFI function.
///
/// Until then, `LoggingControlChannelSender` performs the *encode* half for real
/// (so the wire bytes are exercised and witnessable) and reports the message to
/// the status/log panel instead of putting it on a socket -- the same legitimate
/// stand-in pattern the rest of `MainView` uses for the not-yet-wired channel.
protocol ControlChannelSending {
    /// Send one `ClientMessage` to the daemon. Implementations must encode it as
    /// the `PROTOCOL.md` NDJSON wire form; see `ControlChannelSending.encoded(_:)`.
    func send(_ message: ClientMessage)
}

extension ControlChannelSending {
    /// The exact newline-delimited-JSON wire bytes for `message`, per
    /// `PROTOCOL.md`'s framing ("each message is a single JSON object serialized
    /// on one line, terminated by `\n`"). Shared by every conforming type so the
    /// stand-in and the eventual real transport frame identically -- e.g.
    /// `ClientMessage.stop` encodes to `{"type":"stop"}\n`, byte-for-byte the
    /// `{"type":"stop"}` payload the daemon's `control_channel_probe` asserts on.
    ///
    /// Returns `nil` only if `JSONEncoder` fails, which for these fixed,
    /// finite `Codable` enums cannot happen in practice; callers treat `nil` as
    /// "nothing to send" rather than trapping.
    func encoded(_ message: ClientMessage) -> String? {
        let encoder = JSONEncoder()
        // Deterministic key order so the wire form is stable/inspectable (the
        // daemon parses by key regardless, but a stable form makes logs and
        // packet captures reproducible).
        encoder.outputFormatting = [.sortedKeys]
        guard let data = try? encoder.encode(message),
              let json = String(data: data, encoding: .utf8) else {
            return nil
        }
        return json + "\n"
    }
}

/// The default `ControlChannelSending` used until the real `iroh` transport is
/// wired: it performs the real JSON *encoding* of every `ClientMessage`
/// (exercising the exact wire bytes) and forwards a human-readable line to a
/// caller-supplied sink (the status/log panel) instead of writing to a socket.
///
/// This is deliberately not a no-op: encoding the kill-switch `ClientMessage.stop`
/// to its `{"type":"stop"}` wire form on every Cancel press means the send path is
/// demonstrably live and the wire contract is exercised today, so swapping in the
/// real transport later is a drop-in at `MainView`'s single injection site with no
/// behavioral surprise (the bytes are already correct).
struct LoggingControlChannelSender: ControlChannelSending {
    /// Where a sent message is surfaced. `MainView` passes a closure that appends
    /// a `ServerMessage`-shaped entry to its log panel, so a sent `.stop` shows up
    /// in the same status list as everything else.
    let report: (ClientMessage, _ wire: String) -> Void

    func send(_ message: ClientMessage) {
        guard let wire = encoded(message) else { return }
        report(message, wire)
    }
}
