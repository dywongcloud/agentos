# Pairing verification phrase — cross-platform derivation spec

This document is the **authoritative, byte-exact specification** of the
short pairing-verification phrase (a short-authentication-string / SAS) that
Project Aro PRD **P0-2 / 7.1** requires to be shown on **both** the Mac and
the iPhone during pairing. The iOS half is implemented in this pass
(`ios/Sources/HoloIrohApp/PairingPhrase.swift` +
`PairingWordlist.swift`); the **matching Mac-daemon display is a follow-on**
(see "Daemon side" at the bottom). This spec exists so that follow-on can be
implemented with zero guesswork and provably produce the *same* phrase.

## Why a phrase at all

Scanning/pasting the iroh ticket proves *possession of the ticket*, not
*who is on the other end*. An attacker who can substitute the QR the user
scans (a printed/projected fake, a compromised display) can man-in-the-
middle the pairing. The phrase defeats that: it is derived deterministically
from the ticket and shown on both ends, and the user confirms the two match.
A substituted ticket produces a **different** phrase, so the mismatch is
visible and the user aborts.

This only works if **both ends derive the phrase identically**. Everything
below is chosen to make that trivially reproducible in Rust as well as
Swift.

## The algorithm (version 1)

Given the ticket string:

1. **Canonicalize.** Trim leading/trailing ASCII/Unicode whitespace
   (including a trailing newline). Do **not** alter the ticket body. The
   input to the hash is the canonical string's **UTF-8 bytes**.
   - iOS: `ticket.trimmingCharacters(in: .whitespacesAndNewlines)`.
   - Daemon: hash `ticket.to_string()` directly — it already has no
     surrounding whitespace, so trimming is a no-op on that side and the two
     inputs are identical. (`ticket.to_string()` is exactly what
     `main.rs`'s `print_ticket_qr` encodes into the QR, so the scanned
     bytes equal the daemon's bytes.)

2. **Hash.** `digest = SHA-256(utf8_bytes)` → 32 bytes.
   - iOS: `CryptoKit.SHA256.hash(data:)`. **Not** Swift's `Hasher` /
     `hashValue` — that is per-process seeded and not stable across runs or
     platforms.
   - Daemon: `sha2::Sha256` (the `sha2` crate). Same bytes in → same 32
     bytes out.

3. **Index the wordlist.** The wordlist has **exactly 256 entries**
   (`2^8`), so each digest byte `0…255` maps to exactly one word — no
   modulo, no bit-slicing, no bias. The phrase is the first `N` words
   (`N = 4` by default):

   ```
   phrase = wordlist[digest[0]] ++ " " ++ wordlist[digest[1]]
              ++ " " ++ wordlist[digest[2]] ++ " " ++ wordlist[digest[3]]
   ```

   4 words out of 256 gives `256^4 = 2^32 ≈ 4.3 billion` possible phrases —
   far more than an interactive attacker can grind against a single live
   pairing attempt.

That is the entire algorithm. It is pure and total: SHA-256 is defined for
any byte length (including the empty string), and `N` is clamped to the
digest length, so there is no failing input.

### Version / wordlist are a contract

`PairingPhrase.algorithmVersion` (iOS) and the wordlist's contents+order are
**a shared contract**. Changing the hash, the index rule, the word count,
**or any word / word ordering** is a breaking change: the daemon must embed
the byte-for-byte identical wordlist and bump its own version in lockstep.
The wordlist is the 256 words in
`ios/Sources/HoloIrohApp/PairingWordlist.swift`, in that exact order (index
`i` == digest byte value `i`).

## Known-answer test vectors (version 1)

These were produced by **running the actual Swift `PairingPhrase`**
implementation, and independently reproduced by a portable SHA-256 reference
(Python), so a Rust reimplementation can self-check against them. The
`sha256[0:4]` column is the first four digest bytes (hex) — the only bytes
the 4-word phrase depends on.

| Input ticket string | `sha256[0:4]` | Phrase (4 words) |
|---|---|---|
| `iroh-live:TleiXllmGyIDcEOXtF-AIExJQnPFPlZuzkXmR6OVWNwDAQDAqAFM09EDAQDAqEAB09EDAQDAqP8K09ED/holoiroh` | `7f 41 e7 df` | `grove cover rival quilt` |
| `iroh-live:QTkI9b7mK9JTO8u1DjKCF-5HKeA_8trhtNSq3lo29IYDAQDAqAFMsY8DAQDAqEABsY8DAQDAqP8KsY8D/holoiroh` | `13 c9 5b 37` | `blend patio eagle cliff` |
| `` (empty string) | `e3 b0 c4 42` | `razor mound panda coyote` |
| `not-a-ticket` | `62 d1 ab f0` | `feast pizza metro sedan` |

(The empty-string row is the well-known SHA-256 of the empty input,
`e3b0c442…`, a convenient cross-check that a reimplementation is hashing the
right bytes.)

A Rust reimplementation is correct iff it reproduces this table exactly.

## Daemon side — IMPLEMENTED

The Mac daemon now prints this same phrase next to its QR/ticket at startup
(`main.rs`, right after `print_ticket_qr` / the raw ticket:
`verification phrase (must match the iOS app): ...`), so the user has
something to compare the iPhone's phrase against. It lives in
`mac-daemon/src/pairing_phrase.rs` (`sha2::Sha256` + the identical 256-word
`WORDLIST`), and `examples/pairing_phrase_probe.rs` witnesses byte-for-byte
agreement with the iOS side against this doc's two known-answer vectors
(`grove cover rival quilt`, `blend patio eagle cliff`) via real execution.
Both ends of the SAS mutual-verification loop are therefore live.

The original follow-on recipe that was implemented, kept for reference:

1. Add the `sha2` crate to `mac-daemon/Cargo.toml` (or reuse it if already
   present transitively — check first).
2. Embed the **identical 256-word list** (same order) as a Rust
   `const [&str; 256]`, copied verbatim from `PairingWordlist.swift`. A
   generator/consistency check (e.g. an `examples/pairing_phrase_probe.rs`
   that reproduces the KAT table above) is the right witness per this repo's
   no-unit-tests rule.
3. Add a `pairing_phrase(ticket: &str) -> String`:

   ```rust
   use sha2::{Digest, Sha256};

   fn pairing_phrase(ticket: &str) -> String {
       let digest = Sha256::digest(ticket.trim().as_bytes());
       (0..4).map(|i| WORDLIST[digest[i] as usize]).collect::<Vec<_>>().join(" ")
   }
   ```

4. In `main.rs`, right after `print_ticket_qr(&ticket.to_string()); println!("{ticket}");`,
   print the phrase, e.g.:

   ```rust
   println!("pairing phrase (must match the phrase shown on your iPhone): {}",
            pairing_phrase(&ticket.to_string()));
   ```

5. Verify against the KAT table above (feed the sample ticket strings in and
   confirm the phrases match) before shipping — that is the cross-platform
   agreement check.

Once that lands, the Mac prints e.g. `grove cover rival quilt` and the
iPhone shows the same four words after scanning, and the user confirms they
match. Until it lands, the iOS verification step still functions (it derives
and shows the phrase); it just has nothing on the Mac to compare against
yet, which is the honest current state.

## File map (iOS half, this pass)

- `PairingWordlist.swift` — the fixed 256-word list (the contract).
- `PairingPhrase.swift` — SHA-256 + index derivation (pure, total).
- `PairingTicket.swift` — extract the `iroh-live:…` ticket from a scanned
  QR payload / paste.
- `QRScannerView.swift` — AVFoundation `.qr` scanner (`UIViewRepresentable`).
- `QRScannerSheet.swift` — the scanner sheet + permission-denied fallback.
- `PairingVerificationView.swift` — shows the phrase, gates Connect on an
  explicit "it matches" confirmation.
- `PairingView.swift` — wires Scan QR → auto-fill, and Connect → verify → connect.
