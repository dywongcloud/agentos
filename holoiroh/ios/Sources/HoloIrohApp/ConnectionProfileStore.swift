import Foundation
import SQLite3

/// Stores the ticket and pairing personal identification number (PIN) for a daemon connection.
/// The verification phrase is derived from the ticket and is not stored.
struct ConnectionProfile: Identifiable, Equatable {
    let id: Int64
    var name: String
    var ticket: String
    var pin: String
    var createdAt: Date

    var phrase: String { PairingPhrase.phrase(for: ticket) }
}

/// Stores connection profiles in SQLite.
/// The primary database is in Application Support.
/// If that path fails, the store tries its app Documents directory.
/// The main actor serializes all database access.
/// The in-memory default remains available without a database.
@MainActor
final class ConnectionProfileStore: ObservableObject, ConnectionProfileRepository {
    @Published private(set) var profiles: [ConnectionProfile] = []

    private var db: OpaquePointer?

    /// Contains the path from the most recent database-open attempt.
    /// The value is empty before initialization.
    private(set) var databasePath: String = ""

    /// Indicates whether a SQLite database is open.
    var isOpen: Bool { db != nil }

    /// Makes SQLite copy bound text before Swift releases the temporary C string.
    private static let transient = unsafeBitCast(-1, to: sqlite3_destructor_type.self)

    init(databaseURL: URL? = nil) {
        let primary = databaseURL ?? Self.defaultDatabaseURL()
        // Try the primary path first; fall back to a Documents-dir path if it
        // can't be opened, so USER-saved profiles still persist. (The "Dev Mac"
        // default itself no longer depends on sqlite at all -- reload()
        // synthesizes it in-memory -- but a working DB still matters for
        // everything the user saves.)
        if !openDatabase(primary), databaseURL == nil {
            let fallback = Self.fallbackDatabaseURL()
            NSLog("ConnectionProfileStore: primary db open failed -- falling back to \(fallback.path)")
            _ = openDatabase(fallback)
        }
        if db != nil {
            createTableIfNeeded()
        }
        // reload() ALWAYS runs -- even when db == nil (both sqlite paths failed)
        // -- so the in-memory default is synthesized and `profiles` is never left
        // empty. This is the last line of defense behind the sqlite fallback.
        reload()
        // Launch diagnostic: the macOS harness proves the seeding LOGIC; this
        // line proves the actual DEVICE state (pull it from the console). If a
        // real phone ever shows `opened=false` or `devMac=false`, the root
        // cause is right here rather than in the UI.
        let dev = profiles.first(where: { $0.name == "Dev Mac" })
        NSLog("ConnectionProfileStore: opened=\(db != nil) profiles=\(profiles.count) devMac=\(dev != nil) pin=\(dev?.pin ?? "-")")
    }

    /// Opens the database at `url` and stores its handle.
    /// Returns `false` and logs the SQLite error if opening fails.
    @discardableResult
    private func openDatabase(_ url: URL) -> Bool {
        databasePath = url.path
        var handle: OpaquePointer?
        if sqlite3_open(url.path, &handle) == SQLITE_OK {
            db = handle
            return true
        }
        NSLog("ConnectionProfileStore: failed to open sqlite db at \(url.path): \(String(cString: sqlite3_errmsg(handle)))")
        sqlite3_close(handle)
        db = nil
        return false
    }

    deinit {
        sqlite3_close(db)
    }

    private static func defaultDatabaseURL() -> URL {
        let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("HoloIroh", isDirectory: true)
        try? FileManager.default.createDirectory(at: support, withIntermediateDirectories: true)
        return support.appendingPathComponent("profiles.sqlite")
    }

    /// Returns the fallback database location in the app Documents directory.
    private static func fallbackDatabaseURL() -> URL {
        let docs = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("HoloIroh", isDirectory: true)
        try? FileManager.default.createDirectory(at: docs, withIntermediateDirectories: true)
        return docs.appendingPathComponent("profiles.sqlite")
    }

    private func createTableIfNeeded() {
        exec("""
        CREATE TABLE IF NOT EXISTS profiles (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            ticket TEXT NOT NULL,
            pin TEXT NOT NULL DEFAULT '',
            created_at REAL NOT NULL
        );
        """)
        // Schema hardening (the article's indexing / uniqueness principle): a
        // UNIQUE index on `ticket` enforces at the DB level what save() already
        // enforces in code -- one row per daemon ticket -- as defense in depth.
        // IF NOT EXISTS keeps it idempotent; on a legacy DB that somehow holds
        // duplicate tickets the creation simply fails and is logged (the in-code
        // dedup still protects), so adding it can never break an existing store.
        exec("CREATE UNIQUE INDEX IF NOT EXISTS idx_profiles_ticket ON profiles(ticket);")
    }

    /// Contains the built-in ticket for the synthesized "Dev Mac" profile.
    // A NODE-ID-ONLY ticket, on purpose: the daemon's identity key is stable
    // (~/.holoiroh/iroh_secret), and both the daemon and this app's iroh endpoint
    // use n0's default relay+discovery preset (presets::N0) -- so the phone
    // resolves this node id to the daemon's CURRENT relay + direct paths via pkarr
    // discovery. A full ticket's trailing bytes are ephemeral direct-address hints
    // that drift on every daemon restart; carrying them here made this constant go
    // stale. Stripping to node id + broadcast name gives the drift-proof form that
    // ALWAYS reaches the live daemon and never needs re-syncing.
    //
    // Derived + witnessed live via `mac-daemon/examples/print_current_ticket.rs`
    // (node id 9e15ae39...; CONNECT_OK against the running daemon with PIN 394299).
    private static let currentDevTicket =
        "iroh-live:nhWuOUavJaTyFA2AXzWPTiUUg38hFs6cOjKHKJu9pXwA/holoiroh"
    private static let currentDevPin = "394299"

    /// Identifies the `UserDefaults` entry for a refreshed daemon ticket.
    /// The app writes it after an authenticated control-channel update or manual ticket scan.
    private static let refreshedTicketDefaultsKey = "refreshedDevTicket"

    /// Returns a stored refreshed ticket only when it has the `iroh-live:` scheme.
    private static var storedRefreshedTicket: String? {
        guard let t = UserDefaults.standard.string(forKey: refreshedTicketDefaultsKey),
              t.hasPrefix("iroh-live:")
        else { return nil }
        return t
    }

    /// Returns the refreshed ticket when available.
    /// Otherwise, it returns the built-in ticket.
    static var effectiveDefaultTicket: String {
        storedRefreshedTicket ?? currentDevTicket
    }

    // MARK: - CRUD

    /// Identifies synthesized profiles that do not exist in SQLite.
    /// Deleting this identifier does not remove the synthesized default.
    static let syntheticDefaultID: Int64 = -1

    /// Returns the synthesized "Dev Mac" profile.
    var defaultProfile: ConnectionProfile? {
        profiles.first { $0.id == Self.syntheticDefaultID }
    }

    /// Identifies the `UserDefaults` entry for the most recently connected profile ticket.
    private static let lastConnectedTicketDefaultsKey = "lastConnectedTicket"

    /// Returns the most recently connected saved profile for launch auto-connect.
    /// If that profile is unavailable, it returns the synthesized default.
    var autoConnectProfile: ConnectionProfile? {
        if let lastTicket = UserDefaults.standard.string(forKey: Self.lastConnectedTicketDefaultsKey),
           let match = profiles.first(where: { $0.ticket == lastTicket }) {
            return match
        }
        return defaultProfile
    }

    /// Stores `ticket` as the most recently connected profile for launch auto-connect.
    func markConnected(ticket: String) {
        UserDefaults.standard.set(ticket, forKey: Self.lastConnectedTicketDefaultsKey)
    }

    func reload() {
        // Load any USER-saved profiles from sqlite, best-effort. Crucially this
        // NEVER early-returns before setting `profiles`: the old `guard let db
        // else { return }` (and the prepare-fail early return) left `profiles`
        // empty whenever sqlite couldn't open on device -> the "saved profiles
        // are empty" bug. Everything below always runs.
        var loaded: [ConnectionProfile] = []
        if let db {
            var stmt: OpaquePointer?
            if sqlite3_prepare_v2(db, "SELECT id, name, ticket, pin, created_at FROM profiles ORDER BY created_at DESC;", -1, &stmt, nil) == SQLITE_OK {
                while sqlite3_step(stmt) == SQLITE_ROW {
                    loaded.append(ConnectionProfile(
                        id: sqlite3_column_int64(stmt, 0),
                        name: String(cString: sqlite3_column_text(stmt, 1)),
                        ticket: String(cString: sqlite3_column_text(stmt, 2)),
                        pin: String(cString: sqlite3_column_text(stmt, 3)),
                        createdAt: Date(timeIntervalSince1970: sqlite3_column_double(stmt, 4))
                    ))
                }
            } else {
                logError("prepare select")
            }
            sqlite3_finalize(stmt)
        }

        // The "Dev Mac" default slot is ALWAYS the synthesized current-daemon
        // CONSTANT (currentDevTicket/Pin) -- never a stored (possibly stale or
        // duplicated) sqlite row. So it can't go missing because sqlite failed
        // to open, or a view's @StateObject never initialized -- the real reasons
        // it vanished on device -- and there is never more than one. Drop any
        // persisted "Dev Mac" rows and prepend the constant.
        var result = loaded.filter { $0.name != "Dev Mac" }
        result.insert(
            ConnectionProfile(
                id: Self.syntheticDefaultID,
                name: "Dev Mac",
                ticket: Self.effectiveDefaultTicket,
                pin: Self.currentDevPin,
                createdAt: Date(timeIntervalSince1970: 0)
            ),
            at: 0
        )
        profiles = result
    }

    /// Replaces the synthesized default ticket after a daemon identity change.
    /// The method accepts only a different ticket with the `iroh-live:` scheme.
    /// It persists an accepted ticket and reloads profiles.
    /// Returns `true` when the update succeeds.
    @discardableResult
    func refreshDefaultTicket(_ ticket: String) -> Bool {
        let trimmed = ticket.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed.hasPrefix("iroh-live:"),
              trimmed != defaultProfile?.ticket
        else { return false }
        UserDefaults.standard.set(trimmed, forKey: Self.refreshedTicketDefaultsKey)
        reload()
        return true
    }

    /// Inserts a profile or updates the profile with the same ticket.
    /// Saving the same ticket under a new name renames the existing profile.
    /// Returns `false` when the database is unavailable, the name or ticket is empty, or SQLite fails.
    @discardableResult
    func save(name: String, ticket: String, pin: String) -> Bool {
        guard let db else { return false }
        let trimmedName = name.trimmingCharacters(in: .whitespacesAndNewlines)
        let trimmedTicket = ticket.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedName.isEmpty, !trimmedTicket.isEmpty else { return false }

        var stmt: OpaquePointer?
        let sql: String
        if let existing = profiles.first(where: { $0.ticket == trimmedTicket }) {
            sql = "UPDATE profiles SET name = ?1, pin = ?3 WHERE id = \(existing.id);"
        } else {
            sql = "INSERT INTO profiles (name, ticket, pin, created_at) VALUES (?1, ?2, ?3, ?4);"
        }
        guard sqlite3_prepare_v2(db, sql, -1, &stmt, nil) == SQLITE_OK else {
            logError("prepare save")
            return false
        }
        defer { sqlite3_finalize(stmt) }

        sqlite3_bind_text(stmt, 1, trimmedName, -1, Self.transient)
        sqlite3_bind_text(stmt, 2, trimmedTicket, -1, Self.transient)
        sqlite3_bind_text(stmt, 3, pin.trimmingCharacters(in: .whitespacesAndNewlines), -1, Self.transient)
        sqlite3_bind_double(stmt, 4, Date().timeIntervalSince1970)

        guard sqlite3_step(stmt) == SQLITE_DONE else {
            logError("step save")
            return false
        }
        reload()
        return true
    }

    func delete(_ profile: ConnectionProfile) {
        exec("DELETE FROM profiles WHERE id = \(profile.id);")
        reload()
    }

    // MARK: - Helpers

    private func exec(_ sql: String) {
        guard let db else { return }
        var errMsg: UnsafeMutablePointer<CChar>?
        if sqlite3_exec(db, sql, nil, nil, &errMsg) != SQLITE_OK {
            NSLog("ConnectionProfileStore: exec failed: \(errMsg.map { String(cString: $0) } ?? "unknown")")
            sqlite3_free(errMsg)
        }
    }

    private func logError(_ what: String) {
        guard let db else { return }
        NSLog("ConnectionProfileStore: \(what) failed: \(String(cString: sqlite3_errmsg(db)))")
    }
}
