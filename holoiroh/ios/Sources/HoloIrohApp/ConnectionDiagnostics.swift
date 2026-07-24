import Foundation
import Combine

/// A tiny app-wide diagnostics recorder so on-device reports ("saved profiles
/// are empty", "it won't connect") are SELF-diagnosing -- the hidden
/// `DiagnosticsView` reads this instead of anyone pulling console logs.
///
/// A shared singleton (not injected) because the recording sites are scattered
/// (the connection layer, the ticket-refresh path) and none of them are views.
@MainActor
final class ConnectionDiagnostics: ObservableObject {
    static let shared = ConnectionDiagnostics()

    /// The most recent connection failure, human-readable.
    @Published private(set) var lastError: String?
    /// A short prefix of the ticket that last failed to connect.
    @Published private(set) var lastErrorTicketPrefix: String?
    /// When `lastError` was recorded.
    @Published private(set) var lastErrorAt: Date?
    /// A rolling log of recent connection-relevant events (capped).
    @Published private(set) var log: [String] = []

    private init() {}

    /// Record a failed connect attempt.
    func recordFailure(_ reason: String, ticket: String) {
        lastError = reason
        lastErrorTicketPrefix = String(ticket.prefix(28))
        lastErrorAt = Date()
        append("connect failed: \(reason)")
    }

    /// Record a successful connect.
    func recordConnected(ticket: String) {
        append("connected: \(String(ticket.prefix(28)))…")
    }

    /// Record a ticket refresh (identity rotation picked up over the channel).
    func recordTicketRefresh(from old: String, to new: String) {
        append("default ticket refreshed: \(String(old.prefix(20)))… -> \(String(new.prefix(20)))…")
    }

    /// Record any other noteworthy event.
    func note(_ message: String) { append(message) }

    private func append(_ message: String) {
        let stamp = ConnectionDiagnostics.formatter.string(from: Date())
        log.append("\(stamp)  \(message)")
        if log.count > 50 { log.removeFirst(log.count - 50) }
        // Also mirror to the system log (matches ConnectionProfileStore's own NSLog
        // diagnostics) -- the in-memory `log` above only surfaces once someone opens the
        // hidden DiagnosticsView, but a device console pull (this project's standard
        // screenshot-free iOS witnessing method -- screenshots are blocked on-device) needs
        // these events visible without a human in the loop shaking the phone first.
        NSLog("ConnectionDiagnostics: \(message)")
    }

    private static let formatter: DateFormatter = {
        let f = DateFormatter()
        f.dateFormat = "HH:mm:ss"
        return f
    }()
}
