import Foundation
import Combine

/// Records recent app connection diagnostics for `DiagnosticsView`.
/// The shared instance serves recording sites outside the view hierarchy.
/// Ticket fields store prefixes instead of complete tickets.
@MainActor
final class ConnectionDiagnostics: ObservableObject {
    static let shared = ConnectionDiagnostics()

    /// Contains the most recent connection failure message.
    @Published private(set) var lastError: String?
    /// Contains the first 28 characters of the last failed ticket.
    @Published private(set) var lastErrorTicketPrefix: String?
    /// Contains the time of the most recent connection failure.
    @Published private(set) var lastErrorAt: Date?
    /// Contains at most 50 recent connection events.
    @Published private(set) var log: [String] = []

    private init() {}

    /// Records a failed connection attempt.
    func recordFailure(_ reason: String, ticket: String) {
        lastError = reason
        lastErrorTicketPrefix = String(ticket.prefix(28))
        lastErrorAt = Date()
        append("connect failed: \(reason)")
    }

    /// Records a successful connection.
    func recordConnected(ticket: String) {
        append("connected: \(String(ticket.prefix(28)))…")
    }

    /// Records a ticket refresh received through the control channel.
    func recordTicketRefresh(from old: String, to new: String) {
        append("default ticket refreshed: \(String(old.prefix(20)))… -> \(String(new.prefix(20)))…")
    }

    /// Records another connection event.
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
