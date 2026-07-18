import Foundation

/// Extracts and lightly validates the iroh ticket string out of a decoded
/// QR payload (or a pasted value).
///
/// ## What the QR actually contains
///
/// The Mac daemon renders **exactly the ticket string** into the QR — no
/// URL wrapper, no JSON, no extra fields. `mac-daemon/src/main.rs`'s
/// `print_ticket_qr(&ticket.to_string())` calls `QrCode::new(ticket.as_bytes())`,
/// where `ticket` is a `LiveTicket` whose `to_string()` looks like:
///
/// ```
/// iroh-live:TleiXllmGyIDcEOXtF-AIExJQnPFPlZuzkXmR6OVWNwDAQD…/holoiroh
/// ```
///
/// So in the normal case the decoded QR string *is* the ticket and this
/// type just trims surrounding whitespace. The extra tolerance below exists
/// only to be forgiving of a future QR that wraps the ticket (e.g. some
/// `holoiroh://pair?ticket=…` scheme) or a paste that picked up stray
/// surrounding text — it never *invents* a ticket, it only locates an
/// already-present `iroh-live:` token.
enum PairingTicket {
    /// The scheme prefix every iroh-live ticket carries.
    static let scheme = "iroh-live:"

    /// Returns the canonical ticket string if `raw` contains a plausible
    /// iroh-live ticket, else `nil`.
    ///
    /// Rules, in order:
    /// 1. Trim surrounding whitespace/newlines.
    /// 2. Find the first occurrence of the scheme anywhere in the value
    ///    (covers both the common QR case — the whole payload *is* the
    ///    ticket — and a wrapped/prefixed payload).
    /// 3. Take from the scheme to the end of that **whitespace-delimited
    ///    token**. A real iroh ticket (base32 body + `/holoiroh`) contains
    ///    no whitespace, so stopping at the first whitespace strips any
    ///    trailing text that a paste or wrapper picked up, in both the
    ///    prefix case and the mid-string case.
    /// 4. Reject anything without the scheme, or with no ticket body after
    ///    the scheme.
    ///
    /// This never mutates the ticket body — the extracted substring is
    /// hashed as-is for the verification phrase, so it must match the
    /// daemon's own `ticket.to_string()` byte-for-byte.
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

    /// Convenience predicate: does `raw` contain a usable ticket?
    static func isValid(_ raw: String) -> Bool {
        extract(from: raw) != nil
    }
}
