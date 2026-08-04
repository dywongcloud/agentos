import Foundation
import CryptoKit

/// Derives the pairing verification phrase from a ticket.
///
/// The app and daemon show this phrase during pairing.
/// The user confirms that both phrases match.
///
/// The algorithm has three steps:
/// 1. Trim surrounding whitespace from the ticket.
/// 2. Hash the ticket's UTF-8 bytes with SHA-256.
/// 3. Map leading digest bytes to the fixed 256-word list.
///
/// The daemon must use the same algorithm version and wordlist order.
/// See `ios/PAIRING_PHRASE.md` for known-answer vectors.
enum PairingPhrase {
    /// Identifies the hash, indexing rule, word count, and wordlist contract.
    /// Increment this value when any contract element changes.
    static let algorithmVersion = 1

    /// Sets the default phrase length to four words.
    static let defaultWordCount = 4

    /// Removes surrounding whitespace and newlines from a ticket.
    /// It does not change the ticket body.
    static func canonicalize(_ ticket: String) -> String {
        ticket.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// Returns the SHA-256 digest of the canonical ticket's UTF-8 bytes.
    static func digest(of ticket: String) -> [UInt8] {
        let canonical = canonicalize(ticket)
        let hash = SHA256.hash(data: Data(canonical.utf8))
        return Array(hash)
    }

    /// Maps leading digest bytes to words.
    /// The method clamps `wordCount` to the inclusive range from 0 through 32.
    static func words(for ticket: String, wordCount: Int = defaultWordCount) -> [String] {
        let bytes = digest(of: ticket)
        let n = max(0, min(wordCount, bytes.count))
        return (0..<n).map { PairingWordlist.words[Int(bytes[$0])] }
    }

    /// Returns the selected words joined by spaces.
    static func phrase(for ticket: String, wordCount: Int = defaultWordCount) -> String {
        words(for: ticket, wordCount: wordCount).joined(separator: " ")
    }
}
