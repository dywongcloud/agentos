import Foundation
import CryptoKit

/// Derives the short human-readable **pairing verification phrase** (a
/// short-authentication-string / SAS) shown during pairing.
///
/// ## Why this exists (Project Aro PRD P0-2 / 7.1)
///
/// Scanning or pasting the iroh ticket authenticates *possession of the
/// ticket*, but not *who you're talking to*: an attacker who can substitute
/// the QR the user scans (a projected/printed fake, a compromised screen)
/// can insert themselves in the middle. The defense is a phrase derived
/// deterministically from the ticket and shown on **both** ends — the Mac
/// prints it next to its QR, the iPhone shows it after scanning — and the
/// user visually confirms the two match. A substituted ticket produces a
/// *different* phrase, so the mismatch is visible and the user aborts.
///
/// For this to work the derivation must be **identical and deterministic on
/// both platforms**. That is the entire contract, and it drives every
/// choice below:
///
/// 1. **Input** is the *canonical ticket string* — the exact same bytes the
///    daemon renders into the QR. The daemon encodes `ticket.to_string()`
///    (`iroh-live:<base32>/holoiroh`) into the QR via
///    `QrCode::new(ticket.as_bytes())` (`mac-daemon/src/main.rs
///    print_ticket_qr`). We canonicalize by trimming surrounding
///    whitespace only — the ticket body itself is never mutated — so the
///    scanned string and the daemon's own string hash to the same digest.
///
/// 2. **Hash** is **SHA-256** (via CryptoKit here, `sha2::Sha256` on the
///    Rust daemon). This is deliberately *not* Swift's built-in `Hasher` /
///    `hashValue`, which is seeded with a per-process random value and is
///    explicitly **not** stable across runs or platforms — using it would
///    make the two ends disagree every time. SHA-256 over identical bytes
///    yields identical output everywhere, which is exactly what a
///    cross-platform SAS needs.
///
/// 3. **Words** come from a fixed 256-entry list (`PairingWordlist`). Each
///    digest byte (`0...255`) indexes exactly one word — no modulo bias, no
///    bit-slicing. The phrase is the first `wordCount` words:
///    `word[digest[0]] word[digest[1]] ...`. Four words from 256 gives
///    `256^4 = 2^32 ≈ 4.3 billion` possibilities, far more than an
///    interactive attacker can grind against one live pairing attempt.
///
/// The daemon reproduces this byte-for-byte; see `holoiroh/ios/PAIRING_PHRASE.md`
/// for the authoritative spec and a known-answer test vector.
///
/// This type is **pure**: no I/O, no state, no networking, total over all
/// inputs (SHA-256 is defined for any byte length, including empty). That
/// is what "correct-by-construction" means here — there is nothing to fail
/// at runtime, so the AVFoundation camera path being un-exercisable
/// headlessly does not leave this logic unverified: it is witnessed
/// directly by running the derivation.
enum PairingPhrase {
    /// Bumped whenever the derivation *rule* changes (hash function, index
    /// scheme, word count) OR the wordlist contents/order change
    /// (`PairingWordlist.version`). A daemon and app that disagree on this
    /// number cannot produce matching phrases and should say so rather than
    /// show two phrases that will never line up.
    static let algorithmVersion = 1

    /// Number of words in the rendered phrase. Four is the default: short
    /// enough to read aloud and compare in a second or two, long enough
    /// (2^32) to make a matching-phrase forgery infeasible for an
    /// interactive attacker.
    static let defaultWordCount = 4

    /// Canonicalize a scanned/pasted ticket into the exact byte sequence the
    /// phrase is derived from. Trims surrounding whitespace/newlines (a
    /// pasted or scanned value often carries a trailing newline) but never
    /// alters the ticket body. This must match how the daemon treats its own
    /// ticket string before hashing (the daemon hashes `ticket.to_string()`
    /// with no surrounding whitespace, so trimming here makes the two agree).
    static func canonicalize(_ ticket: String) -> String {
        ticket.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// The raw SHA-256 digest of the canonical ticket bytes. Exposed so the
    /// verification-witness path can show/compare the digest directly.
    static func digest(of ticket: String) -> [UInt8] {
        let canonical = canonicalize(ticket)
        let hash = SHA256.hash(data: Data(canonical.utf8))
        return Array(hash)
    }

    /// The list of words for the phrase (defaults to `defaultWordCount`).
    ///
    /// Total: for any `ticket` (including `""`), `digest` is 32 bytes, and we
    /// read the first `wordCount` of them; `wordCount` is clamped to the
    /// digest length so an over-long request can never read out of bounds.
    static func words(for ticket: String, wordCount: Int = defaultWordCount) -> [String] {
        let bytes = digest(of: ticket)
        let n = max(0, min(wordCount, bytes.count))
        return (0..<n).map { PairingWordlist.words[Int(bytes[$0])] }
    }

    /// The rendered, space-joined verification phrase — the string shown to
    /// the user and compared against the Mac's display.
    static func phrase(for ticket: String, wordCount: Int = defaultWordCount) -> String {
        words(for: ticket, wordCount: wordCount).joined(separator: " ")
    }
}
