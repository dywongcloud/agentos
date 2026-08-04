import Foundation

@MainActor
final class TinfoilVerificationStore: ObservableObject {
    @Published private(set) var verification: TinfoilVerification?

    private var activeSessionID: UUID?
    private var activeProfileIdentity: String?
    private var verificationSessionID: UUID?
    private var verificationProfileIdentity: String?

    func beginSession(id: UUID, profileIdentity: String) {
        activeSessionID = id
        activeProfileIdentity = profileIdentity
        verificationSessionID = nil
        verificationProfileIdentity = nil
        verification = nil
    }

    func update(
        _ verification: TinfoilVerification,
        sessionID: UUID,
        profileIdentity: String
    ) {
        guard sessionID == activeSessionID,
              profileIdentity == activeProfileIdentity
        else { return }
        verificationSessionID = sessionID
        verificationProfileIdentity = profileIdentity
        self.verification = verification
    }

    func isBound(to sessionID: UUID, profileIdentity: String) -> Bool {
        verification != nil &&
            verificationSessionID == sessionID &&
            verificationProfileIdentity == profileIdentity &&
            sessionID == activeSessionID &&
            profileIdentity == activeProfileIdentity
    }

    func reset() {
        activeSessionID = nil
        activeProfileIdentity = nil
        verificationSessionID = nil
        verificationProfileIdentity = nil
        verification = nil
    }
}
