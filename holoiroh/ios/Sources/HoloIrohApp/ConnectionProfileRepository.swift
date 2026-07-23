import Foundation

/// The data-layer contract for connection profiles -- the **Repository pattern**
/// from "Designing Efficient Local-First Architectures with SwiftData" (the
/// article's central abstraction over data access). Depending on this protocol
/// rather than a concrete backend means a future SwiftData- or sync-backed
/// implementation can conform without touching a single view.
///
/// ## How holoiroh maps to the article's local-first principles
/// - **Offline-primary + immediate response.** Profiles live entirely on-device,
///   and the current-daemon "Dev Mac" default is a compile-time constant
///   synthesized in-memory on every load (`ConnectionProfileStore.reload()`), so
///   it is present with zero latency and no network -- the pairing screen never
///   shows an empty list or a connectivity spinner.
/// - **Single source of truth.** One app-level store is created in `HoloIrohApp`
///   and injected via `@EnvironmentObject`, so the launch-time default and
///   `PairingView` share one instance and one sqlite file.
/// - **Separation of concerns.** Model (`ConnectionProfile`, a value type) / Data
///   (this repository protocol) / Backend (`ConnectionProfileStore`, a SQLite
///   local-first implementation) / Presentation (SwiftUI views).
///
/// ## Deliberate non-adoptions (scoped on purpose, per "without breaking things")
/// - **No sync layer / conflict resolution.** Connection profiles are local-only
///   by design -- the daemon connection is the live channel; profiles never
///   leave the device, so there is nothing to synchronize and no conflicts to
///   resolve. The article's Sync layer simply does not apply to this data.
/// - **No SwiftData framework migration.** The raw-SQLite `ConnectionProfileStore`
///   is device-confirmed after a hard-won fix to the always-present-default path;
///   swapping the persistence framework underneath that critical path would risk
///   regressing it. We apply the *principles* (local-first, repository, SSOT,
///   separation) without force-fitting the framework onto a handful-of-rows store
///   where it would add risk and migration surface with no user-visible benefit.
@MainActor
protocol ConnectionProfileRepository: ObservableObject {
    /// The current profiles, default-first. ALWAYS contains the current-daemon
    /// "Dev Mac" default (the local-first guarantee) even if persistence fails.
    var profiles: [ConnectionProfile] { get }

    /// Persist (insert, or update-by-ticket) a user profile. The reserved
    /// "Dev Mac" default slot is owned by the synthesized constant and is not a
    /// user save target. Returns whether the write succeeded.
    @discardableResult
    func save(name: String, ticket: String, pin: String) -> Bool

    /// Remove a profile. Deleting the synthesized default is a harmless no-op
    /// that re-appears on the next load -- it only changes when the daemon's
    /// identity constant changes.
    func delete(_ profile: ConnectionProfile)

    /// Re-read the backing store and re-apply the always-present-default guarantee.
    func reload()
}
