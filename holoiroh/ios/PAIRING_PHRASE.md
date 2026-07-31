# Pairing verification phrase — cross-platform derivation spec

This document is the **authoritative, byte-exact specification** of the
short pairing-verification phrase (a short-authentication-string, or SAS).
Project Aro PRD **P0-2 / 7.1** requires this phrase to appear on **both**
the Mac and the iPhone during pairing. This pass implements the iOS half
(`ios/Sources/HoloIrohApp/PairingPhrase.swift` and
`PairingWordlist.swift`). The **matching Mac-daemon display is a follow-on**
(see "Daemon side" at the bottom). This spec removes guesswork from that
follow-on work, so the follow-on can provably produce the *same* phrase.

## Why a phrase at all

Scanning or pasting the iroh ticket proves *possession of the ticket*, not
*who is on the other end*. An attacker who can substitute the QR the user
scans (a printed or projected fake, a compromised display) can
man-in-the-middle the pairing. The phrase defeats that. It derives
deterministically from the ticket. Both ends show the phrase. The user then
confirms that the two match. A substituted ticket produces a **different**
phrase. The user then sees the mismatch. The user aborts the pairing.

This only works if **both ends derive the phrase identically**. The choices
below make that reproduction trivial in both Rust and Swift.

## The algorithm (version 1)

Given the ticket string:

1. **Canonicalize.** Trim leading and trailing ASCII or Unicode whitespace
   (including a trailing newline). Do **not** alter the ticket body. The
   input to the hash is the canonical string's **UTF-8 bytes**.
   - iOS: `ticket.trimmingCharacters(in: .whitespacesAndNewlines)`.
   - Daemon: hash `ticket.to_string()` directly. It already has no
     surrounding whitespace, so trimming is a no-op on that side. The two
     inputs are therefore identical. (`ticket.to_string()` is exactly what
     `main.rs`'s `print_ticket_qr` encodes into the QR. The scanned bytes
     therefore equal the daemon's bytes.)

2. **Hash.** `digest = SHA-256(utf8_bytes)` → 32 bytes.
   - iOS: `CryptoKit.SHA256.hash(data:)`. **Do not use** Swift's `Hasher`
     or `hashValue`. Both are per-process seeded, so they are not stable
     across runs or platforms.
   - Daemon: `sha2::Sha256` (the `sha2` crate). Same bytes in → same 32
     bytes out.

3. **Index the wordlist.** The wordlist has **exactly 256 entries**
   (`2^8`). Each digest byte `0…255` therefore maps to exactly one word.
   The mapping uses no modulo, no bit-slicing, and no bias. The phrase is
   the first `N` words (`N = 4` by default):

   ```
   phrase = wordlist[digest[0]] ++ " " ++ wordlist[digest[1]]
              ++ " " ++ wordlist[digest[2]] ++ " " ++ wordlist[digest[3]]
   ```

   4 words out of 256 gives `256^4 = 2^32 ≈ 4.3 billion` possible phrases.
   This is far more than an interactive attacker can grind against a single
   live pairing attempt.

That is the entire algorithm. It is pure and total. SHA-256 works for any
byte length, including the empty string. The algorithm also clamps `N` to
the digest length. As a result, no input can fail.

### Version / wordlist are a contract

`PairingPhrase.algorithmVersion` (iOS) and the wordlist's contents and order
are **a shared contract**. Changing the hash, the index rule, the word
count, **or any word or word ordering** is a breaking change. The daemon
must then embed the byte-for-byte identical wordlist. The daemon must also
bump its own version in lockstep.
The wordlist is the 256 words in
`ios/Sources/HoloIrohApp/PairingWordlist.swift`, in that exact order (index
`i` == digest byte value `i`).

## Known-answer test vectors (version 1)

The actual Swift `PairingPhrase` implementation **produced these test
vectors**. A portable SHA-256 reference in Python independently reproduced
them. A Rust reimplementation can therefore self-check against them. The
`sha256[0:4]` column shows the first four digest bytes in hex. The 4-word
phrase depends only on these bytes.

| Input ticket string | `sha256[0:4]` | Phrase (4 words) |
|---|---|---|
| `iroh-live:TleiXllmGyIDcEOXtF-AIExJQnPFPlZuzkXmR6OVWNwDAQDAqAFM09EDAQDAqEAB09EDAQDAqP8K09ED/holoiroh` | `7f 41 e7 df` | `grove cover rival quilt` |
| `iroh-live:QTkI9b7mK9JTO8u1DjKCF-5HKeA_8trhtNSq3lo29IYDAQDAqAFMsY8DAQDAqEABsY8DAQDAqP8KsY8D/holoiroh` | `13 c9 5b 37` | `blend patio eagle cliff` |
| `` (empty string) | `e3 b0 c4 42` | `razor mound panda coyote` |
| `not-a-ticket` | `62 d1 ab f0` | `feast pizza metro sedan` |

(The empty-string row shows the well-known SHA-256 of the empty input,
`e3b0c442…`. This row is a convenient cross-check that confirms a
reimplementation hashes the right bytes.)

A Rust reimplementation is correct iff it reproduces this table exactly.

## Daemon side — IMPLEMENTED

The Mac daemon now prints this same phrase next to its QR and ticket at
startup. It prints the phrase in `main.rs`, right after `print_ticket_qr`
and the raw ticket, as
`verification phrase (must match the iOS app): ...`. The user can then
compare this phrase against the iPhone's phrase. This phrase logic lives in
`mac-daemon/src/pairing_phrase.rs` (`sha2::Sha256` and the identical
256-word `WORDLIST`). `examples/pairing_phrase_probe.rs` witnesses
byte-for-byte agreement with the iOS side. It uses real execution against
this doc's two known-answer vectors (`grove cover rival quilt`,
`blend patio eagle cliff`). Both ends of the SAS mutual-verification loop
are therefore live.

The list below shows the original follow-on recipe. The daemon side above
already implements this recipe. This document keeps the list for reference:

1. Check first whether the `sha2` crate is already present transitively
   in `mac-daemon/Cargo.toml`. If so, reuse it instead of adding a
   duplicate. Otherwise, add the `sha2` crate to `mac-daemon/Cargo.toml`.
2. Embed the **identical 256-word list**, in the same order, as a Rust
   `const [&str; 256]`. Copy it verbatim from `PairingWordlist.swift`. A
   generator or consistency check is the right witness per this repo's
   no-unit-tests rule. For example, `examples/pairing_phrase_probe.rs` can
   reproduce the KAT table above.
3. Add a `pairing_phrase(ticket: &str) -> String`:

   ```rust
   use sha2::{Digest, Sha256};

   fn pairing_phrase(ticket: &str) -> String {
       let digest = Sha256::digest(ticket.trim().as_bytes());
       (0..4).map(|i| WORDLIST[digest[i] as usize]).collect::<Vec<_>>().join(" ")
   }
   ```

4. In `main.rs`, print the phrase right after
   `print_ticket_qr(&ticket.to_string()); println!("{ticket}");`. For
   example:

   ```rust
   println!("pairing phrase (must match the phrase shown on your iPhone): {}",
            pairing_phrase(&ticket.to_string()));
   ```

5. Before shipping, verify the implementation against the KAT table above.
   Feed the sample ticket strings into the implementation. Confirm that
   the output phrases match the table. This check is the cross-platform
   agreement check.

Once that lands, the Mac prints, for example, `grove cover rival quilt`.
The iPhone then shows the same four words after scanning. The user
confirms that the two match. Until it lands, the iOS verification step
still functions: it derives and shows the phrase. It just has nothing on
the Mac to compare against yet. This is the honest current state.

## File map (iOS half, this pass)

- `PairingWordlist.swift` — the fixed 256-word list (the contract).
- `PairingPhrase.swift` — SHA-256 and index derivation (pure, total).
- `PairingTicket.swift` — extract the `iroh-live:…` ticket from a scanned
  QR payload or a paste.
- `QRScannerView.swift` — AVFoundation `.qr` scanner (`UIViewRepresentable`).
- `QRScannerSheet.swift` — the scanner sheet and permission-denied fallback.
- `PairingVerificationView.swift` — shows the phrase, gates Connect on an
  explicit "it matches" confirmation.
- `PairingView.swift` — wires Scan QR → auto-fill, and Connect → verify → connect.
