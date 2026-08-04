# HoloIroh pairing, ticket display, reprinting, and authentication

This document covers four parts of daemon pairing:

- Ticket display as raw text and a Quick Response (QR) code.
- Verification phrase display on the daemon and app.
- Ticket reprinting and identity rotation.
- Authentication with a personal identification number (PIN) and device allowlist.

Project Aro product requirements document (PRD) P0-2 / 7.1 tracks the complete pairing scope.
The scope includes these items:

- QR exchange.
- Phrase verification.
- Keychain storage.
- Cross-device revocation.
- One active controller for each Mac.

The current implementation provides QR exchange and phrase verification.
It also provides a PIN and a persisted daemon allowlist.
The app stores connection profiles in SQLite.
The app persists its iroh identity seed in Keychain.
Cross-device revocation has no user interface (UI).
The daemon supports a revocation data operation.
No command or UI calls that operation.

## Implementation status

| Piece | Status | Where |
|---|---|---|
| Ticket printed as text on startup | **Implemented** | `src/main.rs`, `print_pairing_block()` |
| PIN generated and printed on startup | **Implemented** | `src/main.rs`, `generate_default_pin()` and `println!` |
| `--no-pin-auth` flag | **Implemented** | `src/main.rs`, `Cli::no_pin_auth` |
| Manual 53×53 terminal QR rendering | **Implemented and probe-witnessed** | `src/main.rs`, `print_ticket_qr()`; `examples/qr_probe.rs` |
| App QR scanning and ticket fill | **Implemented** | `QRScannerView.swift`, `QRScannerSheet.swift`, and `PairingView.swift` |
| Matching four-word verification phrase | **Implemented and probe-witnessed on both endpoints** | `src/pairing_phrase.rs`, `PairingPhrase.swift`, `PairingVerificationView.swift`, and `examples/pairing_phrase_probe.rs` |
| PNG rendering through `--qr-png` | **Not implemented** | No current implementation |
| `Allowlist` load, save, contains, add, and remove operations | **Implemented and probe-witnessed** | `src/allowlist.rs`; `examples/allowlist_probe.rs` |
| `verify_pin()` comparison | **Implemented and probe-witnessed** | `src/allowlist.rs`; `examples/allowlist_probe.rs` |
| `generate_pin()` and `generate_default_pin()` | **Implemented and probe-witnessed** | `src/allowlist.rs`; `examples/allowlist_probe.rs` |
| PIN and allowlist gate in control-channel acceptance | **Implemented and end-to-end witnessed** | `ControlChannel::authenticate` in `src/control_channel.rs`; `examples/auth_gate_probe.rs` |
| `ClientMessage::Pin` and `ServerMessage::AuthRejected` | **Implemented** | `src/control_channel.rs`; `PROTOCOL.md` |
| Device revocation with `remove_entry` | **Operation implemented; no command or UI calls it** | `src/allowlist.rs::Allowlist::remove_entry` |
| `--rotate-every <duration>` | **Implemented as current-pairing-block reprinting** | `Cli::rotate_every`, `parse_rotate_duration`, `print_pairing_block()`, and `examples/ticket_rotation_probe.rs` |
| Fresh-keypair rotation on each timer tick | **Not implemented** | Open design gap below |
| App iroh identity-seed storage in Keychain | **Implemented** | App Keychain storage |
| Cross-device revocation | **No UI or propagation implementation** | Remaining PRD P0-2 / 7.1 work |

The authentication gate is compiled Rust, not a proposed design.
Run `cargo run --example auth_gate_probe` to exercise its decisions.
This repository uses executable `examples/*_probe.rs` witnesses instead of `#[test]` files.
The crate has no unit tests.
A live iroh Quick UDP Internet Connections (QUIC) witness also exercised the gate.

The daemon prints a QR code and a verification phrase.
The app scans the QR code and derives the same phrase.
A fresh manual **Connect** action requires phrase confirmation.
A saved-profile direct connection bypasses the phrase-confirmation screen.

`--rotate-every` reprints the current pairing block.
The block contains the current ticket, QR code, and phrase.
The option does not create a new ticket or rotate identity.
It does not invalidate a captured ticket.

## Startup output

The current startup pairing block uses this order:

1. Header.
2. Terminal QR code.
3. Raw ticket.
4. Verification phrase.
5. PIN, unless `--no-pin-auth` is active.

The following earlier witness captured only the raw ticket and PIN lines:

```
$ ./target/debug/holoiroh-daemon
iroh-live:QTkI9b7mK9JTO8u1DjKCF-5HKeA_8trhtNSq3lo29IYDAQDAqAFMsY8DAQDAqEABsY8DAQDAqP8KsY8D/holoiroh
pairing PIN (first connection only): 945351
```

With `--no-pin-auth`, the PIN line changes:

```
$ ./target/debug/holoiroh-daemon --no-pin-auth
iroh-live:pG2Hcljv0cPCDgskTxrGG2PtqYjN1bLoxFg_gKwFgncDAQDAqAFMxocDAQDAqEABxocDAQDAqP8KxocD/holoiroh
PIN auth disabled (--no-pin-auth): any device with the ticket can connect
```

A real build session produced these lines.
They are not invented examples.
Current code also prints the QR code and verification phrase around these lines.

## QR pairing

### Current daemon behavior

The ticket exceeds 100 characters.
It uses iroh's self-describing ticket format.
Manual entry between a Mac terminal and an iPhone is error-prone.
The QR code carries exactly `ticket.to_string()` as bytes.
It has no Uniform Resource Locator (URL) or JavaScript Object Notation (JSON) wrapper.

`print_ticket_qr()` manually renders a 53×53 terminal QR code.
The current path does not use the `qrcode` crate renderer.
The renderer uses terminal block characters.
It retains the required quiet zone.

`examples/qr_probe.rs` witnesses this path.
A realistic ticket of approximately 180 characters produces a well-formed, scannable 53×53 QR grid.
Current daemon comments also account for approximately 230-byte tickets.
The current renderer has a fixed 53×53 size.
QR construction failure logs an error and does not stop startup.
The raw ticket remains the fallback.

During the earlier review, `mac-daemon/Cargo.toml` included `qrcode` version `0.14` with default features disabled.
That review covered approximately 230-byte tickets.
It found 53×53 or 57×57 modules at error-correction level L.
The reviewed application programming interface (API) used `QrCode::new()` and a generic `render()` method.
The review considered `qrcode::render::unicode::Dense1x2`.
That renderer packs two vertical modules into one terminal row.
It uses Unicode half-block glyphs `▀`, `▄`, ` `, and `█`.

The review used this baseline API shape:

```rust
use qrcode::QrCode;

let code = QrCode::new(ticket.to_string().as_bytes())?;
let rendered = code.render()
    .light_color(' ')   // background module
    .dark_color('█')    // foreground module -- unicode full block for a
                         // denser, more scannable terminal QR than plain '#'
    .build();
println!("{rendered}");
```

The baseline renders one module for each character.
The reviewed `Dense1x2` option differed from that baseline.
Neither reviewed crate-rendering path describes the current manual renderer.

The `qrcode` crate's published `0.14.1` release dates from July 2024.
The crate uses the MIT and Apache-2.0 licenses.
The original API review classified it as actively maintained.

### Current app behavior

`PairingView` presents `QRScannerSheet` from its **Scan QR code** button.
`QRScannerView` requests camera permission.
It scans AVFoundation `.qr` metadata.
The scanner returns raw decoded text.
`PairingTicket.extract(from:)` finds the first `iroh-live:` token.
It trims surrounding whitespace and trailing whitespace-delimited text.
It never changes the ticket body.

A successful scan fills the ticket field.
It does not connect automatically.
For a fresh manual connection, the user selects **Connect**.
This action opens `PairingVerificationView`.
The view displays the derived four-word phrase.
The user must select **It matches — connect** before the app calls `onConnect`.
A mismatch or cancellation abandons that connection attempt.
A saved-profile direct connection does not show this confirmation screen.

If camera permission is denied, the scanner sheet shows recovery guidance.
The user can cancel and paste the ticket.

### PNG status

Portable Network Graphics (PNG) output is not implemented.
There is no `--qr-png` flag.
The current terminal QR path does not require a PNG image feature.

The earlier crate review confirmed PNG support through `render::<Luma<u8>>()`.
It recorded this example:

```rust
use image::Luma;

let code = QrCode::new(ticket.to_string().as_bytes())?;
let image = code.render::<Luma<u8>>().build();
image.save("/tmp/holoiroh-ticket-qr.png")?;
std::process::Command::new("open")
    .arg("/tmp/holoiroh-ticket-qr.png")
    .spawn()?; // opens in Preview.app on macOS -- `open` is the standard
               // macOS CLI for "open this file in its default app"
```

The review identified `qrcode = { version = "0.14", default-features = false, features = ["image"] }` as the minimal specification.
It also identified `/tmp/holoiroh-ticket-qr.png` as an example output path.
The standard macOS `open` command can open that file in Preview.

PNG output could avoid terminal block-character distortion.
It could also provide a stable Preview window for screen sharing.
It would not require a control-channel, allowlist, or wire-protocol change.
These are historical review facts, not a current implementation recipe.

## Verification phrase

The phrase algorithm is version 1.
`ios/PAIRING_PHRASE.md` defines the byte-exact contract.
Both endpoints hash the ticket's 8-bit Unicode Transformation Format (UTF-8) bytes with Secure Hash Algorithm 256-bit (SHA-256).
They map the first four digest bytes into the same ordered 256-word list.

The daemon prints this text after its ticket:
`verification phrase (must match the iOS app): ...`.
The app shows the same four words in `PairingVerificationView`.
`examples/pairing_phrase_probe.rs` checks two known-answer vectors.
It witnesses `grove cover rival quilt` and `blend patio eagle cliff`.

The phrase supports visual comparison of the displayed ticket.
It is not a mutual challenge-and-acknowledgment protocol.

## Ticket reprinting and identity rotation

### Identity change on restart

`Live::from_env()` reads `IROH_SECRET` when that variable is set.
If it is unset, `Live::from_env()` generates a fresh iroh keypair at process start.
A fresh keypair creates a fresh node identifier (ID) and ticket.
Identity changes on restart when `IROH_SECRET` is unset.

Setting `IROH_SECRET` keeps a stable daemon identity and ticket across restarts.
This avoids repeated pairing in a trusted, long-running deployment.
The allowlist recognizes a device by its node ID.
It does not identify the device by the daemon's ticket.

Current deployment code can also persist the daemon identity in `~/.holoiroh/iroh_secret`.
Operators must determine whether their launch path exports that identity as `IROH_SECRET`.

### `--rotate-every <duration>`

`Cli::rotate_every` is implemented in `src/main.rs`.
`src/duration.rs::parse_rotate_duration` parses its value without `humantime`.
It accepts `30m`, `2h`, `90s`, and `1h30m`.

The option is defined as follows:

```rust
/// Re-print the pairing ticket + verification phrase on a fixed interval
/// while the daemon keeps running (e.g. `30m`, `2h`, `1h30m`), so a stale
/// QR screenshot stops being the one the operator is reading off.
#[arg(long, value_parser = duration::parse_rotate_duration)]
rotate_every: Option<std::time::Duration>,
```

A `tokio::select!` loop waits for shutdown or a timer tick.
The code skips the interval's immediate first tick.
This prevents duplicate startup output.
It uses `MissedTickBehavior::Skip` to prevent catch-up bursts.

Each later tick calls `print_pairing_block()` with the current ticket.
Startup and timer output use the same rendering function.
Only the header label differs.
The QR code, raw ticket, and phrase remain identical.

`examples/ticket_rotation_probe.rs` witnesses duration parsing.
Run it with `cargo run --example ticket_rotation_probe`.
The probe also checks stable phrase derivation for one ticket.
It checks that a distinct ticket produces a distinct phrase.

The timer does not mint a fresh keypair or `LiveTicket`.
It does not rotate identity.
It does not invalidate a leaked or captured ticket.
It only refreshes the operator's display.

### Identity-rotation design gap

A new ticket does not disconnect a client that already used the old ticket.
Iroh establishes each connection when the peer dials.
An open `Connection` remains active after another ticket appears or disappears.

Rebuilding the underlying `Endpoint` is different.
That action drops existing connections.
It is a larger operational change than reprinting the current pairing block.

The earlier investigation distinguished two rotation models:

- **Ticket-string rotation only.**
  - This model does not affect connected clients.
  - An old ticket must become invalid before this model can block its later use.
  - The current timer does not perform that invalidation.
  - A true implementation requires `Live` or `iroh_moq` republishing under a fresh identity.
  - The investigation did not verify support without active-stream disruption.
- **Full identity rotation through an `Endpoint` rebuild.**
  - This model drops existing connections.
  - It acts like a scheduled self-restart.
  - A supervisor can restart the daemon every `N` minutes.
  - `N` is the configured restart interval.
  - This model requires no new daemon code.
  - Each restart loses in-flight `holo_bridge` state.

Full identity rotation has unresolved lifecycle behavior.
It affects the `live`, `router`, `endpoint`, and `LocalBroadcast` objects.
It also affects the `holo_bridge` subprocess and active control-channel connection.

A rebuild witness also requires a live daemon run after macOS privacy preflight.
Transparency, Consent, and Control (TCC) governs this preflight.
Screen Recording and Accessibility grants gate the publish path.
Project tracking records this action as `holoiroh-user-action-grant-tcc-and-run-daemon`.

These facts describe the open design gap.
They do not describe implemented identity rotation.

## Authentication beyond ticket possession

Anyone with the ticket can reach the daemon's control endpoint.
A leaked QR image or visible terminal can disclose that ticket.
Holo can perform arbitrary computer-use automation.
The PIN and allowlist add a gate after ticket-based dialing.

### PIN

`allowlist::generate_default_pin()` produces a six-digit numeric PIN.
The daemon creates a fresh PIN on each run by default.
It prints the PIN after the pairing block.
`--no-pin-auth` disables this gate for local development or testing.
The daemon never persists the generated PIN.
It persists only the successfully paired device ID.

`HOLOIROH_PIN` provides a stable PIN override for development Macs.
The daemon uses its exact value instead of generating a random PIN.
The environment variable keeps the PIN out of `ps` command output.

Saved app profiles store the ticket and PIN in SQLite.
The app stores its iroh identity seed in Keychain.
A random per-run PIN makes a saved PIN stale after daemon restart.
An allowlisted device bypasses the PIN gate.
Therefore, a stale saved PIN does not affect that device.
A fresh app install using a saved profile still needs the current PIN.

### Persisted allowlist

The daemon stores its allowlist at `~/.holoiroh/allowlist.json`.
`allowlist::Allowlist` implements load, save, lookup, add, and remove operations.
`cargo run --example allowlist_probe` witnesses these operations.

Each `AllowlistEntry` records these fields:

- `device_id`: The connecting peer's iroh node ID as hexadecimal text.
- Optional `label`.
- `paired_at`: A Unix timestamp.

A missing file loads as an empty allowlist.
This is the normal first-run state.
Corrupt JSON causes a hard error.
The probe asserts that `load(corrupt) -> is_err`.
The daemon fails closed for corrupt allowlist data.

### Control-channel gate

`ProtocolHandler::accept` calls `ControlChannel::authenticate` for every accepted connection.
The gate runs before the daemon sends `control channel ready`.

The gate uses these rules:

1. If PIN authentication is disabled, accept immediately.
2. If the device ID is allowlisted, accept immediately.
3. Otherwise, require this first line: `{"type":"pin","pin":"<candidate>"}`.
4. Reject a prompt, malformed JSON, or premature stream closure.
5. If the PIN matches, add and save the device immediately.
6. On later connections, that device bypasses PIN entry.
7. If authentication fails, send `ServerMessage::AuthRejected { text }`.
8. Then close with `connection.close(0, b"auth rejected")`.

A rejected peer receives no greeting or bridge function.
`verify_pin` uses an exclusive OR (XOR) fold comparison.
This avoids the early-exit signal from a direct `==` comparison.
Its documentation states the precise threat-model limits.

### Wire additions

The protocol adds variants without changing existing variants.
This follows the extension policy in `PROTOCOL.md`.

```json
// iOS -> Mac, first message from an unrecognized device
{ "type": "pin", "pin": "123456" }

// Mac -> iOS, sent instead of the greeting when auth fails
{ "type": "auth_rejected", "text": "incorrect PIN" }
```

### PIN generation limit

`generate_pin()` uses `std::collections::hash_map::RandomState`.
It does not use a dedicated cryptographically secure pseudorandom number generator (CSPRNG) crate.
The standard library reseeds `RandomState` from the operating-system random source on each call.
Its implementation uses `getrandom(2)` or `SecRandomCopyBytes` transitively.

This choice avoids another dependency for a short-lived, single-use, six-digit PIN.
The PIN also requires prior ticket possession.
It is not a long-term cryptographic secret.

A longer-lived credential would require `rand::rngs::OsRng` or an equivalent source.
The same requirement applies if pairing stops requiring ticket possession.
These conditions document the current security limit.

### Device revocation

`Allowlist::remove_entry(device_id)` removes a paired device from stored data.
The `allowlist_probe` case named `remove_entry revokes a previously paired device` witnesses that operation.
No operator-facing feature calls it.

Current omissions are:

- No `--revoke-device <id>` command-line flag.
- No control-channel revocation message.
- No kill-switch integration.
- No cross-device revocation UI.
- No cross-device revocation propagation.

Removing an entry does not disconnect an active device.
The gate runs once for each new connection during `accept()`.
The daemon has no active-connection registry.
Immediate revocation requires such a registry or daemon shutdown through the kill switch.

## End-to-end authentication witness

The component witnesses use these commands:

- `cargo run --example auth_gate_probe`
- `cargo run --example control_channel_probe`

The first command exercises `ControlChannel::authenticate` decisions.
The second command exercises the wire protocol.

A separate witness used `examples/control_probe.rs` against a running daemon.
It used a stub `holo serve` backend so the control channel could mount.
The run used a real iroh QUIC connection.

```
=== Run A: fixed identity, no PIN -> reject ===
connected: remote=3f726f895c
-> {"type":"prompt","text":"control_probe: attempting without a PIN"} (no PIN presented)
Error: connection lost
Caused by:
    closed by peer: auth rejected (code 0)

=== Run B: SAME fixed identity, correct PIN -> accept + allowlist ===
connected: remote=3f726f895c
-> {"type":"pin","pin":"055653"}
<- {"type":"status","text":"control channel ready"}
-> {"type":"prompt","text":"control_probe: hello from a real iroh dial (post-PIN)"}
<- {"type":"ack"}
control_probe: OK -- PIN accepted, greeting + ack witnessed over a real iroh connection

=== allowlist now ===
{
  "entries": [
    { "device_id": "8f44da0c66", "paired_at": 1784345619 }
  ]
}

=== Run C: SAME fixed identity, NO pin this time -> succeeds via allowlist ===
connected: remote=3f726f895c
-> {"type":"prompt","text":"control_probe: attempting without a PIN"} (no PIN presented)
<- {"type":"status","text":"control channel ready"}
```

Run C receives the greeting without a PIN.
This demonstrates the allowlist path for a previously paired device.

The temporary harness still expected the old pre-pairing behavior.
It panicked on that obsolete assertion.
The daemon returned the correct `{"type":"status","text":"control channel ready"}` response.
The panic was in the temporary harness, not the daemon.

The committed `examples/control_probe.rs` does not include `PROBE_SECRET_HEX`.
The witness used that local mechanism to keep the same device ID across three runs.
A normal invocation creates a fresh identity.
That default supports independent accept and reject probes.
The fixed-identity mechanism was not committed.

The live witness observed all three authentication outcomes:

- Rejection with `auth_rejected`.
- Acceptance after the correct PIN.
- Acceptance from an existing allowlist entry.

## Open gaps

- PNG QR output is absent.
- Full identity rotation is absent.
- Active-device revocation is absent.
- Cross-device revocation UI and propagation are absent.
- Full identity rotation has unresolved active-connection and `holo_bridge` behavior.
- The TCC-gated daemon run has not witnessed full identity rotation.

The implemented QR and phrase flow remains unchanged unless the shared protocol changes.
