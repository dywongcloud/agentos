import Foundation
import Security

enum IrohIdentityStoreError: LocalizedError {
    case keychain(operation: String, status: OSStatus)
    case malformedSeed(length: Int)
    case unexpectedItemType
    case randomGeneration(status: OSStatus)

    var errorDescription: String? {
        switch self {
        case .keychain(let operation, let status):
            let detail = SecCopyErrorMessageString(status, nil) as String? ?? "OSStatus \(status)"
            return "Iroh identity \(operation) failed: \(detail)"
        case .malformedSeed(let length):
            return "Persisted Iroh identity is malformed: expected 32 bytes, found \(length)"
        case .unexpectedItemType:
            return "Persisted Iroh identity has an unexpected Keychain value type"
        case .randomGeneration(let status):
            return "Secure Iroh identity generation failed with OSStatus \(status)"
        }
    }
}

struct IrohIdentityStore: Sendable {
    static let seedLength = 32

    private static let service = "com.holoiroh.HoloIroh.iroh-identity"
    private static let account = "default-v1"

    func loadOrCreateSeed() throws -> Data {
        if let seed = try readSeed() {
            return seed
        }
        return try createSeed()
    }

    private func readSeed() throws -> Data? {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: Self.service,
            kSecAttrAccount: Self.account,
            kSecReturnData: true,
            kSecMatchLimit: kSecMatchLimitOne,
        ]
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        if status == errSecItemNotFound {
            return nil
        }
        guard status == errSecSuccess else {
            throw IrohIdentityStoreError.keychain(operation: "read", status: status)
        }
        guard let seed = item as? Data else {
            throw IrohIdentityStoreError.unexpectedItemType
        }
        guard seed.count == Self.seedLength else {
            throw IrohIdentityStoreError.malformedSeed(length: seed.count)
        }
        return seed
    }

    private func createSeed() throws -> Data {
        var bytes = [UInt8](repeating: 0, count: Self.seedLength)
        let randomStatus = bytes.withUnsafeMutableBytes { buffer in
            SecRandomCopyBytes(kSecRandomDefault, buffer.count, buffer.baseAddress!)
        }
        guard randomStatus == errSecSuccess else {
            throw IrohIdentityStoreError.randomGeneration(status: randomStatus)
        }
        let candidate = Data(bytes)
        let add: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: Self.service,
            kSecAttrAccount: Self.account,
            kSecAttrAccessible: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            kSecValueData: candidate,
        ]
        let status = SecItemAdd(add as CFDictionary, nil)
        if status == errSecSuccess {
            return candidate
        }
        if status == errSecDuplicateItem {
            guard let winner = try readSeed() else {
                throw IrohIdentityStoreError.keychain(
                    operation: "duplicate-race re-read",
                    status: errSecItemNotFound
                )
            }
            return winner
        }
        throw IrohIdentityStoreError.keychain(operation: "create", status: status)
    }
}

final class IrohEndpointCoordinator: @unchecked Sendable {
    static let shared = IrohEndpointCoordinator()

    private let condition = NSCondition()
    private var endpointIsLive = false

    private init() {}

    func acquire() {
        condition.lock()
        while endpointIsLive {
            condition.wait()
        }
        endpointIsLive = true
        condition.unlock()
    }

    func release() {
        condition.lock()
        endpointIsLive = false
        condition.broadcast()
        condition.unlock()
    }

    func withLease<T>(_ operation: () -> T) -> T {
        acquire()
        defer { release() }
        return operation()
    }
}
