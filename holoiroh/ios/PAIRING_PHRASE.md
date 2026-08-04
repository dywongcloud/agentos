# Pairing verification phrase: cross-platform derivation specification

This document specifies the byte-exact short authentication string (SAS).
Project Aro PRD P0-2 / 7.1 requires the Mac and iPhone to show this phrase.
The app and daemon both implement version 1.

The app implementation is in these files:

- `ios/Sources/HoloIrohApp/PairingPhrase.swift`
- `ios/Sources/HoloIrohApp/PairingWordlist.swift`
- `ios/Sources/HoloIrohApp/PairingVerificationView.swift`

The daemon implementation is in `mac-daemon/src/pairing_phrase.rs`.
The daemon prints the phrase with the ticket and Quick Response (QR) code.

## Purpose

Scanning or pasting the ticket proves ticket possession.
It does not identify the endpoint that displayed the ticket.
An attacker could substitute a printed, projected, or compromised QR code.

Both endpoints derive a phrase from the ticket.
The user compares the phrases before connecting.
A substituted ticket produces a different phrase unless its first four digest bytes collide.
If the phrases differ, the user must cancel pairing.

Both implementations must derive the phrase identically.
The rules below define that shared result.

## Version 1 algorithm

Given the ticket string, use these steps:

1. **Canonicalize the app input.**
   Trim leading and trailing ASCII or Unicode whitespace.
   This includes a trailing newline.
   Do not change the ticket body.
   Hash the canonical string's UTF-8 bytes.
   - App: `ticket.trimmingCharacters(in: .whitespacesAndNewlines)`.
   - Daemon: hash `ticket.to_string()` directly.
   - `ticket.to_string()` has no surrounding whitespace.
   - `main.rs` puts these exact bytes in the QR code.
   - The scanned ticket bytes therefore equal the daemon input bytes.

2. **Hash the bytes.**
   Compute `digest = SHA-256(utf8_bytes)` to produce 32 bytes.
   - App: use `CryptoKit.SHA256.hash(data:)`.
   - Do not use Swift `Hasher` or `hashValue`.
   - Those values use per-process seeds and are not stable across runs or platforms.
   - Daemon: use `sha2::Sha256` from the `sha2` crate.

3. **Index the wordlist.**
   The wordlist contains exactly 256 entries (`2^8`).
   Each digest byte from `0` through `255` maps to one word.
   This mapping uses no modulo, bit slicing, or bias.
   Use the first `N` words.
   The default is `N = 4`.

   ```
   phrase = wordlist[digest[0]] ++ " " ++ wordlist[digest[1]]
              ++ " " ++ wordlist[digest[2]] ++ " " ++ wordlist[digest[3]]
   ```

Four words provide `256^4 = 2^32 ≈ 4.3 billion` possible phrases.
The default pairing flow always uses four words.
SHA-256 accepts every byte length, including zero bytes.

The app clamps an explicit word count to the inclusive range `0...32`.
The daemon clamps an explicit word count to the inclusive range `1...32`.
This difference does not affect the default four-word pairing flow.

## Version and wordlist contract

`PairingPhrase.algorithmVersion` is `1` in the app.
The daemon implements the same version 1 contract without an algorithm-version constant.
The hash, index rule, word count, wordlist contents, and word order form one shared contract.
Changing any contract element is a breaking change.
Both implementations must change together.
A breaking change must add or update matching version metadata on both endpoints.

The canonical wordlist has 256 words.
It is in `ios/Sources/HoloIrohApp/PairingWordlist.swift`.
Index `i` contains the word for digest byte value `i`.
The daemon embeds the identical ordered list in `mac-daemon/src/pairing_phrase.rs`.

## Known-answer vectors for version 1

The Swift implementation produced these vectors.
A portable Python SHA-256 reference reproduced them independently.
`examples/pairing_phrase_probe.rs` checks the daemon against the first two ticket vectors.
The `sha256[0:4]` column gives the first four digest bytes in hexadecimal.
Only those bytes determine the default phrase.

| Input ticket string | `sha256[0:4]` | Phrase (4 words) |
|---|---|---|
| `iroh-live:TleiXllmGyIDcEOXtF-AIExJQnPFPlZuzkXmR6OVWNwDAQDAqAFM09EDAQDAqEAB09EDAQDAqP8K09ED/holoiroh` | `7f 41 e7 df` | `grove cover rival quilt` |
| `iroh-live:QTkI9b7mK9JTO8u1DjKCF-5HKeA_8trhtNSq3lo29IYDAQDAqAFMsY8DAQDAqEABsY8DAQDAqP8KsY8D/holoiroh` | `13 c9 5b 37` | `blend patio eagle cliff` |
| `` (empty string) | `e3 b0 c4 42` | `razor mound panda coyote` |
| `not-a-ticket` | `62 d1 ab f0` | `feast pizza metro sedan` |

The empty-input digest starts with the standard `e3b0c442…` value.
This vector detects an incorrect input or hash operation.
A compatible implementation must reproduce the table exactly.

## Current daemon behavior

At startup, `main.rs` calls `print_pairing_block()`.
This function performs these actions:

1. It renders the ticket with `print_ticket_qr()`.
2. It prints the raw ticket.
3. It prints `verification phrase (must match the iOS app): ...`.

`mac-daemon/src/pairing_phrase.rs` uses `sha2::Sha256`.
It contains the same 256-word `WORDLIST`.
`examples/pairing_phrase_probe.rs` witnesses byte-for-byte agreement for these phrases:

- `grove cover rival quilt`
- `blend patio eagle cliff`

Run the witness with this command:

```sh
cargo run --example pairing_phrase_probe
```

The daemon also reprints the same pairing block on each `--rotate-every` tick.
That option reprints the current ticket.
It does not create a new ticket or phrase.

## Current app behavior

`PairingView` accepts a pasted ticket or opens `QRScannerSheet`.
The scanner returns decoded QR text.
`PairingTicket.extract(from:)` locates the first `iroh-live:` token.
It removes surrounding text at whitespace boundaries.
It does not change the ticket body.

The manual **Connect** action presents `PairingVerificationView`.
That view derives and displays the four-word phrase.
It blocks the connection until the user selects **It matches — connect**.
**It doesn't match** and **Cancel** abandon that connection attempt.

The scanner does not connect automatically after a successful scan.
It fills the ticket field and returns to `PairingView`.
A saved-profile selection connects directly and does not show the verification view again.

## Rust reference implementation

The current daemon uses the same operations as this reference for its ticket input:

```rust
use sha2::{Digest, Sha256};

fn pairing_phrase(ticket: &str) -> String {
    let digest = Sha256::digest(ticket.trim().as_bytes());
    (0..4).map(|i| WORDLIST[digest[i] as usize]).collect::<Vec<_>>().join(" ")
}
```

The production daemon hashes `LiveTicket::to_string()` without calling `trim()`.
That string has no surrounding whitespace.
The resulting four-word phrase therefore matches this reference.

The daemon prints the phrase after the QR code and raw ticket.
The output is equivalent to this example:

```rust
println!("pairing phrase (must match the phrase shown on your iPhone): {}",
         pairing_phrase(&ticket.to_string()));
```

Before changing the contract, run the known-answer witness.
Confirm that both implementations produce the table values.

## File map

- `PairingWordlist.swift`: fixed 256-word contract list.
- `PairingPhrase.swift`: SHA-256 and word-index derivation.
- `PairingTicket.swift`: ticket extraction from scanned or pasted text.
- `QRScannerView.swift`: AVFoundation QR scanner through `UIViewRepresentable`.
- `QRScannerSheet.swift`: scanner sheet and permission-denied fallback.
- `PairingVerificationView.swift`: phrase display and explicit confirmation gate.
- `PairingView.swift`: QR scan, ticket fill, verification, and connection flow.
- `mac-daemon/src/pairing_phrase.rs`: matching Rust phrase derivation and wordlist.
- `mac-daemon/src/main.rs`: QR, raw-ticket, and verification-phrase output.
