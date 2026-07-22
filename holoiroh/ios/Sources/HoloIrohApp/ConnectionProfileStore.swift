import Foundation
import SQLite3

/// A saved way to reach a daemon: the iroh ticket plus the pairing PIN.
/// The verification phrase is NOT stored -- it is always derived from the
/// ticket via `PairingPhrase`, so a stored profile can never show a phrase
/// that disagrees with the ticket it would actually connect to.
struct ConnectionProfile: Identifiable, Equatable {
    let id: Int64
    var name: String
    var ticket: String
    var pin: String
    var createdAt: Date

    var phrase: String { PairingPhrase.phrase(for: ticket) }
}

/// SQLite-backed store for connection profiles, using the system `SQLite3`
/// C module directly (no third-party dependency; profiles are one small
/// table). The database lives in Application Support so it survives app
/// updates but is excluded from the user's visible Documents.
///
/// All sqlite access happens on the main actor -- the table is tiny and
/// every call site is UI-driven, so a serial background queue would add
/// complexity without a measurable win.
@MainActor
final class ConnectionProfileStore: ObservableObject {
    @Published private(set) var profiles: [ConnectionProfile] = []

    private var db: OpaquePointer?

    /// SQLITE_TRANSIENT: tells sqlite to copy bound text immediately.
    /// Swift's temporary C strings from `withCString`/`-1` binds die before
    /// sqlite3_step without this; the classic silent-garbage bug.
    private static let transient = unsafeBitCast(-1, to: sqlite3_destructor_type.self)

    init(databaseURL: URL? = nil) {
        let primary = databaseURL ?? Self.defaultDatabaseURL()
        // Try the primary path first. If it can't be opened -- which would
        // otherwise leave the store SILENTLY EMPTY (db == nil ->
        // `ensureDefaultProfile` early-returns, so the default profile is never
        // seeded, the exact "no saved profile on device" symptom) -- fall back
        // to a Documents-dir path so the default is still seeded somewhere.
        if !openDatabase(primary), databaseURL == nil {
            let fallback = Self.fallbackDatabaseURL()
            NSLog("ConnectionProfileStore: primary db open failed -- falling back to \(fallback.path)")
            _ = openDatabase(fallback)
        }
        if db != nil {
            createTableIfNeeded()
            reload()
            ensureDefaultProfile()
        }
        // Launch diagnostic: the macOS harness proves the seeding LOGIC; this
        // line proves the actual DEVICE state (pull it from the console). If a
        // real phone ever shows `opened=false` or `devMac=false`, the root
        // cause is right here rather than in the UI.
        let dev = profiles.first(where: { $0.name == "Dev Mac" })
        NSLog("ConnectionProfileStore: opened=\(db != nil) profiles=\(profiles.count) devMac=\(dev != nil) pin=\(dev?.pin ?? "-")")
    }

    /// Opens `url` into `db`; returns whether it succeeded (and logs the
    /// concrete sqlite error on failure).
    @discardableResult
    private func openDatabase(_ url: URL) -> Bool {
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

    /// Last-resort DB location if Application Support can't be opened: the
    /// Documents dir (always present + writable in the app sandbox). Keeping
    /// the default profile seeded matters more than the exact file location.
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
    }

    /// The built-in ticket/PIN for the dev Mac's daemon -- the values
    /// [`ensureDefaultProfile`] guarantees are on hand when no working "Dev
    /// Mac" row exists.
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

    /// The iroh node-id portion of a ticket: the first 43 characters after
    /// the `iroh-live:` scheme (a 32-byte node key, base64url unpadded).
    /// Everything after it is address/relay hint data that drifts per daemon
    /// restart; the node id IS the daemon's identity.
    private static func nodeIdPrefix(of ticket: String) -> Substring? {
        let scheme = "iroh-live:"
        guard ticket.hasPrefix(scheme) else { return nil }
        let body = ticket.dropFirst(scheme.count)
        guard body.count >= 43 else { return nil }
        return body.prefix(43)
    }

    /// Guarantees the "Dev Mac" default profile EXISTS on every launch --
    /// deterministically, with no one-time flags. This replaced the old
    /// seed-once-then-refresh-once design after it was live-witnessed
    /// leaving a real phone with an EMPTY profiles table: the UserDefaults
    /// seed flag survives app upgrades, the row had been deleted, and the
    /// refresh path only updated an existing row -- an unreachable state no
    /// flag-bump could repair. The requirement is that the current daemon's
    /// ticket/PIN profile is ALWAYS present, so:
    ///
    /// - no "Dev Mac" row -> insert the built-in constants, every launch
    ///   that finds it missing;
    /// - row exists with the SAME iroh node id as the constants -> leave it
    ///   (its address hints may be fresher; `upsertDefaultProfile` pins it
    ///   to whatever actually connects);
    /// - row exists pointing at a DIFFERENT node id -> repoint the default
    ///   slot at the dev daemon's identity.
    private func ensureDefaultProfile() {
        guard db != nil else { return }
        if let existing = profiles.first(where: { $0.name == "Dev Mac" }) {
            let existingNode = Self.nodeIdPrefix(of: existing.ticket)
            let builtinNode = Self.nodeIdPrefix(of: Self.currentDevTicket)
            if existingNode != nil && existingNode == builtinNode {
                return
            }
            NSLog("ConnectionProfileStore: 'Dev Mac' pointed at a different/invalid node id -- repointing at the built-in daemon identity")
            upsertDefaultProfile(ticket: Self.currentDevTicket, pin: Self.currentDevPin)
        } else {
            NSLog("ConnectionProfileStore: default 'Dev Mac' profile missing -- inserting the built-in daemon identity")
            save(name: "Dev Mac", ticket: Self.currentDevTicket, pin: Self.currentDevPin)
        }
    }

    /// Pins the "Dev Mac" default profile to the ticket/PIN a control-channel
    /// connection just SUCCEEDED with -- called from `MainView` on every
    /// `.connected`. This is what keeps the sqlite default profile pointed at
    /// the daemon's CURRENT identity without hand-syncing the seed constants
    /// after every daemon restart (a ticket's trailing direct-address hints
    /// drift per restart; whatever actually connected is by definition the
    /// freshest working value). Updates by NAME ("refresh the default slot",
    /// same semantics `ensureDefaultProfile` repoints by); inserts the row if a
    /// user deleted it and then connected manually. No-ops when the stored
    /// values already match, so routine reconnects don't churn the DB.
    func upsertDefaultProfile(ticket: String, pin: String) {
        let trimmedTicket = ticket.trimmingCharacters(in: .whitespacesAndNewlines)
        let trimmedPin = pin.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedTicket.isEmpty else { return }
        guard let db else { return }

        if let existing = profiles.first(where: { $0.name == "Dev Mac" }) {
            if existing.ticket == trimmedTicket && existing.pin == trimmedPin { return }
            var stmt: OpaquePointer?
            let sql = "UPDATE profiles SET ticket = ?1, pin = ?2 WHERE name = 'Dev Mac';"
            guard sqlite3_prepare_v2(db, sql, -1, &stmt, nil) == SQLITE_OK else {
                logError("prepare upsert default profile")
                return
            }
            defer { sqlite3_finalize(stmt) }
            sqlite3_bind_text(stmt, 1, trimmedTicket, -1, Self.transient)
            sqlite3_bind_text(stmt, 2, trimmedPin, -1, Self.transient)
            guard sqlite3_step(stmt) == SQLITE_DONE else {
                logError("step upsert default profile")
                return
            }
            reload()
        } else {
            save(name: "Dev Mac", ticket: trimmedTicket, pin: trimmedPin)
        }
        NSLog("ConnectionProfileStore: default 'Dev Mac' profile pinned to the connected daemon identity")
    }

    // MARK: - CRUD

    func reload() {
        guard let db else { return }
        var stmt: OpaquePointer?
        guard sqlite3_prepare_v2(db, "SELECT id, name, ticket, pin, created_at FROM profiles ORDER BY created_at DESC;", -1, &stmt, nil) == SQLITE_OK else {
            logError("prepare select")
            return
        }
        defer { sqlite3_finalize(stmt) }

        var loaded: [ConnectionProfile] = []
        while sqlite3_step(stmt) == SQLITE_ROW {
            loaded.append(ConnectionProfile(
                id: sqlite3_column_int64(stmt, 0),
                name: String(cString: sqlite3_column_text(stmt, 1)),
                ticket: String(cString: sqlite3_column_text(stmt, 2)),
                pin: String(cString: sqlite3_column_text(stmt, 3)),
                createdAt: Date(timeIntervalSince1970: sqlite3_column_double(stmt, 4))
            ))
        }
        profiles = loaded
    }

    /// Inserts (or, if a profile with the same ticket already exists,
    /// updates) a profile. Keying dedup on the ticket rather than the name
    /// means re-saving the same daemon under a new label renames it instead
    /// of quietly accumulating duplicates that all dial the same machine.
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
