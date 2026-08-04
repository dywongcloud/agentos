import Foundation

/// Defines storage operations for connection profiles.
///
/// The current repository stores user profiles in SQLite.
/// It also synthesizes the current daemon profile during each reload.
/// The main actor owns all repository access.
@MainActor
protocol ConnectionProfileRepository: ObservableObject {
    /// Contains the current profiles with the synthesized daemon profile first.
    /// The synthesized profile remains available if persistent storage fails.
    var profiles: [ConnectionProfile] { get }

    /// Inserts a profile or updates the profile with the same ticket.
    /// The synthesized daemon profile is not a save target.
    /// Returns `true` when persistence succeeds.
    @discardableResult
    func save(name: String, ticket: String, pin: String) -> Bool

    /// Deletes a user profile.
    /// Deleting the synthesized daemon profile has no effect.
    func delete(_ profile: ConnectionProfile)

    /// Reloads persistent profiles and restores the synthesized daemon profile.
    func reload()
}
