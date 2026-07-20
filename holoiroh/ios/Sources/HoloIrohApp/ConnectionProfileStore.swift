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
        let url = databaseURL ?? Self.defaultDatabaseURL()
        var handle: OpaquePointer?
        if sqlite3_open(url.path, &handle) == SQLITE_OK {
            db = handle
            createTableIfNeeded()
            seedDevProfileIfNeeded()
            reload()
            refreshDevProfileIfPresent()
        } else {
            NSLog("ConnectionProfileStore: failed to open sqlite db at \(url.path): \(String(cString: sqlite3_errmsg(handle)))")
            sqlite3_close(handle)
            db = nil
        }
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

    /// The development profile for the Mac daemon, seeded exactly once.
    /// Guarded by a UserDefaults flag rather than INSERT OR IGNORE so a
    /// deliberately deleted profile stays deleted on later launches.
    private func seedDevProfileIfNeeded() {
        let seedFlag = "ConnectionProfileStore.didSeedDevProfile"
        guard !UserDefaults.standard.bool(forKey: seedFlag) else { return }
        save(
            name: "Dev Mac",
            ticket: Self.currentDevTicket,
            pin: Self.currentDevPin
        )
        UserDefaults.standard.set(true, forKey: seedFlag)
    }

    /// The ticket/PIN a fresh install seeds as "Dev Mac" -- kept as named constants (rather
    /// than inlined) so [`refreshDevProfileIfPresent`] can reuse the exact same values to
    /// update an ALREADY-seeded install's row in place, without duplicating the literals.
    private static let currentDevTicket =
        "iroh-live:nhWuOUavJaTyFA2AXzWPTiUUg38hFs6cOjKHKJu9pXwCAQDAqAFMmOwDAQDAqEABmOwD/holoiroh"
    private static let currentDevPin = "394299"

    /// One-time in-place refresh of an ALREADY-seeded "Dev Mac" row's ticket/PIN to the
    /// current daemon identity, for installs where `seedDevProfileIfNeeded` already ran (and
    /// so the seed flag blocks re-seeding) against a now-stale ticket. Unlike `save`, which
    /// dedups by TICKET (a changed ticket would insert a second row, not replace the first),
    /// this updates by NAME -- "replace the current default profile" means the same named
    /// slot, refreshed, not an additional saved connection. Guarded by its own one-time flag
    /// so it never fights a user who has since renamed or intentionally repointed "Dev Mac".
    func refreshDevProfileIfPresent() {
        let refreshFlag = "ConnectionProfileStore.didRefreshDevProfile_2026-07-20"
        guard !UserDefaults.standard.bool(forKey: refreshFlag) else { return }
        defer { UserDefaults.standard.set(true, forKey: refreshFlag) }
        guard let db else { return }
        guard profiles.contains(where: { $0.name == "Dev Mac" }) else { return }
        var stmt: OpaquePointer?
        let sql = "UPDATE profiles SET ticket = ?1, pin = ?2 WHERE name = 'Dev Mac';"
        guard sqlite3_prepare_v2(db, sql, -1, &stmt, nil) == SQLITE_OK else {
            logError("prepare refresh dev profile")
            return
        }
        defer { sqlite3_finalize(stmt) }
        sqlite3_bind_text(stmt, 1, Self.currentDevTicket, -1, Self.transient)
        sqlite3_bind_text(stmt, 2, Self.currentDevPin, -1, Self.transient)
        if sqlite3_step(stmt) != SQLITE_DONE {
            logError("step refresh dev profile")
            return
        }
        reload()
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
