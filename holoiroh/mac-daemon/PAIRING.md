# HoloIroh pairing, QR/ticket UX, rotation, and auth-beyond-ticket

This document covers four related areas of `mac-daemon`'s pairing story:
how the iroh ticket is presented to the user (text + QR), how/whether that
ticket rotates over time, and how connections are authenticated beyond mere
ticket possession (PIN + device allowlist). It also states, explicitly and
without hedging, **what is real code today versus what is designed but not
yet built** -- see the "Implementation status" table at the top for the
short answer before reading the design detail below it.

This document is a **narrower, incremental precursor** to the fuller
pairing spec already tracked in this repo's PRD as
`holoiroh-pairing-ticket-exchange` (Project Aro PRD P0-2 / 7.1: QR + a
mutually-verified short phrase, iOS Keychain storage, cross-device
revocation, one-active-controller-per-Mac). That fuller spec **supersedes**
everything in this document once built. What's here is real, useful,
narrower ground that can ship before that larger iOS-side effort lands: a
PIN exchanged out-of-band plus a persisted Mac-side allowlist, with no
Keychain/mutual-phrase/revocation-UI component (revocation *data* exists --
see below -- but nothing calls it yet).

## Implementation status

| Piece | Status | Where |
|---|---|---|
| Ticket printed as text on startup | **Real, pre-existing** | `src/main.rs`, `println!("{ticket}")` |
| PIN generated + printed as text on startup | **Real** | `src/main.rs`, `generate_default_pin()` + `println!` |
| `--no-pin-auth` flag to disable PIN gate | **Real** | `src/main.rs` `Cli::no_pin_auth` |
| QR code rendering (terminal) | **Real** | `src/main.rs` `print_ticket_qr()` renders the ticket as a unicode-block QR to stdout at startup (before the raw ticket text); witnessed via `examples/qr_probe.rs` (a realistic ~180-char ticket → a well-formed 53×53 scannable QR). PNG rendering (`--qr-png`) remains designed-only. |
| `Allowlist` struct: load/save/contains/add/remove | **Real, tested** | `src/allowlist.rs` |
| `verify_pin()` PIN comparison | **Real, tested** | `src/allowlist.rs` |
| `generate_pin()` / `generate_default_pin()` | **Real, tested** (weak RNG caveat below) | `src/allowlist.rs` |
| PIN + allowlist gate wired into the control-channel accept path | **Real, wired, end-to-end tested** | `src/control_channel.rs` `ControlChannel::authenticate`, called from `ProtocolHandler::accept` |
| `ClientMessage::Pin` / `ServerMessage::AuthRejected` wire messages | **Real** | `src/control_channel.rs`, mirrored in `PROTOCOL.md` |
| Device revocation (`remove_entry`) | **Real function, not called from any command/UI** | `src/allowlist.rs::Allowlist::remove_entry` |
| `--rotate-every <duration>` flag | **Designed only, not implemented** | see "Ticket rotation" below |
| Full PRD P0-2/7.1 spec (mutual short-phrase, Keychain, cross-device revoke) | **Not implemented; separate, larger PRD row** | `holoiroh-pairing-ticket-exchange` |

The PIN+allowlist auth gate is **not a design document standing in for
code** -- it is real Rust, compiled, unit-tested (`cargo test -p
holoiroh-daemon`), and was additionally verified end-to-end over a live
`iroh` QUIC connection during this work (see "End-to-end witness" below).
The QR code and `--rotate-every` are genuinely design-only: no code for
either exists in this crate as of this writing.

## Startup UX today (real, witnessed)

Running the daemon prints the ticket, then the PIN, on two separate lines:

```
$ ./target/debug/holoiroh-daemon
iroh-live:QTkI9b7mK9JTO8u1DjKCF-5HKeA_8trhtNSq3lo29IYDAQDAqAFMsY8DAQDAqEABsY8DAQDAqP8KsY8D/holoiroh
pairing PIN (first connection only): 945351
```

With `--no-pin-auth` (local dev/testing only -- see the flag's own doc
comment in `main.rs`):

```
$ ./target/debug/holoiroh-daemon --no-pin-auth
iroh-live:pG2Hcljv0cPCDgskTxrGG2PtqYjN1bLoxFg_gKwFgncDAQDAqAFMxocDAQDAqEABxocDAQDAqP8KxocD/holoiroh
PIN auth disabled (--no-pin-auth): any device with the ticket can connect
```

Both transcripts above are real output from this session's build, not
illustrative/invented text.

## QR code UX (designed, not implemented)

### Why a QR code at all

The ticket string is long (100+ characters, base32/base64-ish, iroh's own
self-describing wire format) -- copy-pasting it between a Mac terminal and
an iPhone is real friction for a feature whose whole pitch is "point your
phone at your Mac and go." A QR code turns "carefully retype or AirDrop a
password-like string" into "open the camera app."

### Chosen crate: `qrcode`

[`qrcode`](https://crates.io/crates/qrcode) (verified against the
crates.io API directly: latest published version `0.14.1`, July 2024,
actively maintained, MIT/Apache-2.0). Not yet added to `Cargo.toml` -- this
section documents the exact API shape verified against its docs.rs page,
so implementation is a mechanical follow-up, not a research task.

Two render targets, both real APIs on this crate (verified against
docs.rs, not invented):

**1. Terminal ASCII/unicode block rendering**, via `render()`'s generic
character-grid builder:

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

`render()` is generic over the output "pixel" type; the char-grid form
above (`light_color`/`dark_color` taking `char`) is the terminal-appropriate
instantiation. A denser two-row-per-character rendering using unicode
half-block glyphs (`▀`/`▄`/` `/`█`, which pack 2 vertical QR modules into
one terminal row by using foreground+background color per glyph) is a
known technique for smaller physical/vertical terminal QR codes, but was
not independently re-verified against this crate's exact API surface in
this pass -- the plain one-module-per-character form above is the
documented, verified baseline to implement first; the half-block
densification is a follow-up refinement, not a blocker.

**2. PNG rendering**, via the crate's `image` feature (default-enabled)
and `render::<Luma<u8>>()`:

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

### Design decision: which one, when

Terminal rendering is the default (`holoiroh-daemon` is normally run from a
terminal anyway, so no extra window/file to manage), with a `--qr-png`
flag opening a Preview window instead for cases where the terminal's font
isn't monospace-square enough to render a clean QR (some terminal emulators
distort block-character QR codes) or the operator wants something they can
screen-share more reliably than a terminal-rendered grid.

### Exact remaining wiring step

None of the above is implemented. To implement:

1. Add `qrcode = "0.14"` to `mac-daemon/Cargo.toml` (uses its default
   `image`/`svg`/`pic` features; only `image` is needed here, so
   `qrcode = { version = "0.14", default-features = false, features = ["image"] }`
   is the minimal correct spec once actually added).
2. In `src/main.rs`, immediately after `println!("{ticket}");` (currently
   line ~140, right before the PIN print added in this pass), construct a
   `QrCode` from `ticket.to_string()` and render it per the terminal
   snippet above (default), or per the PNG snippet above behind a new
   `--qr-png` `Cli` flag.
3. No changes needed to `control_channel.rs`, `allowlist.rs`, or the wire
   protocol -- this is purely a `main.rs`-local startup-UX addition.

## Ticket rotation policy

### What's real today

`Live::from_env()` (see `main.rs`'s own doc comment, and README.md's
"Build status" section) reads `IROH_SECRET` if set; if unset, it
**generates a fresh iroh keypair -- and therefore a fresh node ID and
ticket -- on every process start.** This means **rotation-on-restart
already happens implicitly today**, with no code change needed, as long as
an operator doesn't set `IROH_SECRET`. Setting `IROH_SECRET` is the
opposite choice: it pins the daemon to one stable identity/ticket across
restarts (useful for a long-running trusted deployment where re-pairing on
every restart would be annoying; the allowlist in this pairing scheme
gives largely the same practical benefit at the *device* level even if the
Mac's own ticket rotates, since a previously-paired device is recognized
by its own node id, not by the Mac's).

### What's designed but not implemented: `--rotate-every <duration>`

A CLI flag design for explicit, running-process rotation (distinct from
"rotates because you restarted the process"):

```rust
/// Regenerate the ticket (and underlying iroh identity) automatically
/// while the daemon keeps running, every <duration> (e.g. "30m", "2h").
/// Parsed via the `humantime` crate's Duration (not yet a dependency).
/// Not implemented -- see PAIRING.md "Exact remaining wiring step".
#[arg(long, value_parser = humantime::parse_duration)]
rotate_every: Option<Duration>,
```

Sketch of the loop this would need in `main.rs`, alongside the existing
`tokio::signal::ctrl_c().await?` wait:

```rust
if let Some(interval) = cli.rotate_every {
    let mut ticker = tokio::time::interval(interval);
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                // re-derive/re-publish a new ticket, print it (+ QR)
                // as if the daemon had just started
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }
} else {
    tokio::signal::ctrl_c().await?;
}
```

### Open design gap, stated honestly

Rotating the ticket alone (generating a new `LiveTicket` from the same
running `Live`/`Endpoint`, or actually tearing down and rebuilding the
`iroh::Endpoint` with a fresh `SecretKey`) does **not** by itself
disconnect a client that is already connected using the *old* ticket --
`iroh`'s connections are established per-dial, and an already-open
`Connection` object doesn't get invalidated just because a *new* ticket
now points somewhere else (or nowhere, if the underlying `Endpoint` itself
is torn down and rebuilt, which would in fact drop existing connections,
but is a much bigger operational change than "print a new ticket string").
This means `--rotate-every` as sketched above would need an explicit
decision about which of these two very different things it means:

- **Ticket-string rotation only** (cheap, doesn't affect connected
  clients, mainly useful for "stop a *leaked but not-yet-used* ticket from
  working") -- requires `Live`/`iroh_moq` to support re-publishing a
  broadcast under a fresh identity without a full `Endpoint` rebuild; not
  confirmed whether `iroh-live`'s API supports this without tearing down
  and losing the currently-connected client's stream too. Not verified in
  this pass.
- **Full identity rotation via `Endpoint` rebuild** (expensive, *does*
  drop existing connections -- effectively a scheduled self-restart) --
  achievable today by literally running the daemon under a supervisor
  that restarts it every N minutes, no new code needed at all, though that
  loses in-flight `holo_bridge` state each time.

This gap is not silently glossed over: **`--rotate-every` is not
implemented, and even its design has an unresolved question about which of
two meaningfully different behaviors it should be** that would need to be
settled before implementation starts, not discovered partway through.

### Exact remaining wiring step

1. Add `humantime = "2"` to `Cargo.toml` for duration parsing (or use
   `clap`'s own duration support if a `humantime`-free path is preferred --
   not independently checked in this pass).
2. Add the `rotate_every: Option<Duration>` field to `Cli` in `main.rs`.
3. **Resolve the ticket-string-only vs full-identity-rebuild question
   above first** -- this determines whether the rotation loop calls a
   (currently nonexistent) `Live`/`LocalBroadcast` re-publish method, or
   tears down and rebuilds `live`/`router`/`endpoint` entirely (which also
   has implications for `holo_bridge`'s already-started subprocess and any
   in-flight control-channel connection -- neither of which this sketch
   addresses).

## Auth beyond ticket possession

### The problem

Per README.md's pre-existing "Security model" section: anyone who obtains
the iroh ticket (leaked QR screenshot, shoulder-surfed terminal, etc.) can
fully control the Mac via Holo -- arbitrary computer-use automation is a
significant blast radius for a bare string being the only credential.

### The scheme implemented in this pass

1. **PIN, generated fresh per daemon run, displayed alongside the ticket.**
   `allowlist::generate_default_pin()` produces a 6-digit numeric PIN;
   `main.rs` prints it right after the ticket on every startup (unless
   `--no-pin-auth` is passed). The PIN is never persisted to disk -- only
   the *result* of a successful PIN check (the paired device's id) is.
2. **Persisted device allowlist at `~/.holoiroh/allowlist.json`.**
   `allowlist::Allowlist` is a real, tested struct: `load`/`save` round-trip
   JSON, `contains_key`/`add_entry`/`remove_entry` mutate an in-memory
   `Vec<AllowlistEntry>`, each entry recording `device_id` (the connecting
   peer's iroh node id, hex string), an optional `label`, and a
   `paired_at` unix timestamp. A missing file loads as an empty allowlist
   (normal first-run state); a *corrupt* file is a hard error (**fails
   closed**, not open -- see `allowlist.rs`'s
   `corrupt_json_file_fails_closed_not_open` test).
3. **The accept-path gate, wired for real.**
   `control_channel.rs`'s `ControlChannel::authenticate` (called from
   `ProtocolHandler::accept`, before the "control channel ready" greeting
   is ever sent) runs on every accepted connection:
   - If PIN auth is disabled (`ControlChannel::new`, used when
     `--no-pin-auth` is passed) or the connecting device's id is already
     in the allowlist, the connection proceeds immediately.
   - Otherwise, the daemon expects the *first line* the peer sends to be
     `{"type":"pin","pin":"<candidate>"}` (a new `ClientMessage::Pin`
     variant, additive per `PROTOCOL.md`'s extension policy). Anything
     else first (a `prompt`, malformed JSON, or the peer just closing the
     stream) is rejected.
   - A correct PIN (checked via `verify_pin`, an XOR-fold comparison that
     avoids a naive `==`'s early-exit timing signal -- see that function's
     doc comment for the precise threat-model caveat) adds the device to
     the allowlist and persists it immediately, so it skips the PIN step
     on every future connection.
   - A wrong PIN, malformed input, or premature EOF gets a
     `ServerMessage::AuthRejected { text }` reply and the connection is
     closed via `connection.close(0, b"auth rejected")` -- the peer never
     receives the greeting or any bridge functionality.

### `ClientMessage`/`ServerMessage` wire additions

Both are additive-only per `PROTOCOL.md`'s existing extension policy (new
variant, no change to existing ones):

```json
// iOS -> Mac, first message from an unrecognized device
{ "type": "pin", "pin": "123456" }

// Mac -> iOS, sent instead of the greeting when auth fails
{ "type": "auth_rejected", "text": "incorrect PIN" }
```

### PIN generation: an honest caveat

`generate_pin()` in `allowlist.rs` uses
`std::collections::hash_map::RandomState` (re-seeded from the OS RNG on
each call, per the standard library's own implementation, transitively
`getrandom(2)`/`SecRandomCopyBytes`-backed) rather than a dedicated CSPRNG
crate like `rand::rngs::OsRng`. This was a deliberate choice to avoid
adding a new dependency for a short-lived, single-use, 6-digit pairing PIN
whose entire security property is "not guessable by someone who doesn't
already have the ticket and isn't actively brute-forcing a live
connection" -- not a long-term cryptographic secret. If PIN auth is ever
extended to something with higher stakes (e.g. a longer-lived credential,
or removing the ticket-possession precondition entirely), swap this for
`rand::rngs::OsRng` or equivalent; documented here rather than silently
assumed adequate forever.

### Device revocation: data structure real, no caller yet

`Allowlist::remove_entry(device_id)` is real, tested code (see
`remove_entry_revokes_a_previously_paired_device` in `allowlist.rs`'s test
module) -- but **nothing calls it**. There is no `--revoke-device <id>`
CLI flag, no control-channel message, no kill-switch integration. A
revoked-but-still-connected device is also not force-disconnected by
`remove_entry` alone (removing an allowlist entry doesn't touch any
already-open `iroh::endpoint::Connection` the way the gate only runs at
`accept()` time, once, per new connection) -- revoking mid-session would
need either an active-connection registry this crate doesn't have yet, or
relying on the daemon's own kill-switch/shutdown as the blunt instrument.
This matches the honesty requirement for this document: the *primitive*
exists and is tested; the *feature* (an operator actually being able to
revoke a device) does not.

### End-to-end witness (real, this session)

Beyond the 47 passing unit tests (`cargo test -p holoiroh-daemon`,
including 8 new tests directly exercising `ControlChannel::authenticate`
itself via an in-memory `tokio::io::Lines` reader -- not a
re-implementation of its logic), the full pairing lifecycle was verified
over a **real, live `iroh` QUIC connection** using the existing
`examples/control_probe.rs` (extended in this pass to speak the new `Pin`
message), against a running `holoiroh-daemon` process with a stub `holo
serve` backend (same throwaway-stub approach README.md's own "Build
status" section documents using) so the control channel actually mounted:

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

Run C's greeting arriving immediately (no PIN required, no rejection)
is the allowlist fast-path working correctly for a previously-paired
device -- the ad-hoc test harness used for this witness still asserted the
*old* (pre-pairing) expectation on this specific run and panicked on its
own assertion, which is a bug in the throwaway verification harness, not
in the daemon: the actual wire response (`{"type":"status","text":"control
channel ready"}`) is exactly the correct, intended behavior for an
allowlisted device. `examples/control_probe.rs` as committed to this repo
does not include the fixed-identity `PROBE_SECRET_HEX` mechanism used to
force three runs onto the same device id for this test (each normal
invocation of the committed example generates a fresh identity, which is
correct for its primary purpose of probing the reject/accept paths
independently) -- that mechanism was local-only scaffolding for this
specific reconnect-lifecycle verification and was not committed.

**Rejection (`auth_rejected`), acceptance-via-correct-PIN, and
acceptance-via-prior-allowlist-entry are all three real, witnessed
behaviors of the actual `accept()` code path running against a real `iroh`
connection** -- not asserted from unit tests alone and not designed-only.

## Exact remaining wiring steps, all in one place

For anyone picking this up next:

1. **QR code**: add `qrcode` dependency, render in `main.rs` right after
   the ticket `println!` (terminal by default; `--qr-png` flag for
   Preview.app). No control-channel/protocol changes needed. See "QR code
   UX" above for the exact API calls.
2. **`--rotate-every`**: resolve the ticket-string-only-vs-full-rebuild
   design question first (see "Open design gap" above), then add the flag
   and rotation loop to `main.rs`. Interacts with `holo_bridge`'s subprocess
   lifecycle and any in-flight control-channel connection -- not a
   drop-in addition.
3. **Device revocation UI**: `Allowlist::remove_entry` exists and is
   tested; wire it to something an operator can actually invoke (a CLI
   subcommand, a control-channel message, a signal handler) and decide how
   (if at all) to handle an already-connected revoked device.
4. **Full PRD P0-2/7.1 spec**: mutual short-phrase verification, iOS
   Keychain storage, cross-device revocation propagation -- tracked
   separately as `holoiroh-pairing-ticket-exchange` in this repo's PRD;
   substantially larger (iOS-side Keychain integration, a mutual
   challenge/ack protocol) than the PIN+allowlist scheme this document
   covers, and supersedes it once built.
