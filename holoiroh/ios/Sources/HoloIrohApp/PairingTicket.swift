import Foundation

/// Extracts a plausible iroh ticket from a Quick Response (QR) payload or pasted value.
/// The daemon normally encodes only the ticket string.
/// Extraction also accepts surrounding text that contains an `iroh-live:` token.
/// It trims surrounding whitespace but does not alter the ticket body.
enum PairingTicket {
    /// Identifies the prefix for every iroh-live ticket.
    static let scheme = "iroh-live:"

    /// Returns the first plausible iroh ticket in `raw`.
    ///
    /// Processing order:
    /// 1. Trim surrounding whitespace and newlines.
    /// 2. Find the first scheme occurrence.
    /// 3. Take the whitespace-delimited token that starts at the scheme.
    /// 4. Reject an empty value, missing scheme, or missing ticket body.
    ///
    /// The method does not change the extracted body.
    /// This preserves byte equality for verification-phrase hashing.
    static func extract(from raw: String) -> String? {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        guard let range = trimmed.range(of: scheme) else { return nil }

        // From the scheme onward, take up to the first whitespace so trailing
        // junk after the ticket token is dropped. A ticket never contains
        // whitespace, so this is exact for a real ticket and defensive for a
        // wrapped/padded one.
        let fromScheme = trimmed[range.lowerBound...]
        let token = fromScheme.split(whereSeparator: { $0.isWhitespace }).first
        let candidate = token.map(String.init) ?? String(fromScheme)

        // Must have a non-empty body after the scheme to be a real ticket.
        guard candidate.count > scheme.count else { return nil }
        return candidate
    }

    /// Returns `true` when `raw` contains a usable ticket.
    static func isValid(_ raw: String) -> Bool {
        extract(from: raw) != nil
    }
}
