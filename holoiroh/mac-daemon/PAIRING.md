# HoloIroh pairing, QR/ticket UX, rotation, and auth-beyond-ticket

This document covers four related areas of `mac-daemon`'s pairing story:

- How the daemon presents the iroh ticket to the user, as text and as a QR code.
- Whether and how that ticket rotates over time.
- How the daemon authenticates connections beyond mere ticket possession, using a PIN and a device allowlist.

It also states, explicitly and without hedging, **what is real code today
versus what is designed but not yet built**. See the "Implementation
status" table at the top for the short answer. Read the design detail
below the table for more information.

This document is a **narrower, incremental precursor** to the fuller
pairing spec. This repo's PRD already tracks that fuller spec as
`holoiroh-pairing-ticket-exchange` (Project Aro PRD P0-2 / 7.1). That entry
covers a QR code, a mutually-verified short phrase, iOS Keychain storage,
cross-device revocation, and one-active-controller-per-Mac support. That
fuller spec **supersedes** everything in this document once built. This
document instead covers narrower, real, useful ground that can ship before
that larger iOS-side effort lands. It covers a PIN exchanged out-of-band
and a persisted Mac-side allowlist. It has no Keychain component, no
mutual-phrase component, and no revocation-UI component. Revocation *data*
already exists (see below), but nothing calls it yet.

## Implementation status

| Piece | Status | Where |
|---|---|---|
| Ticket printed as text on startup | **Real, pre-existing** | `src/main.rs`, `println!("{ticket}")` |
| PIN generated + printed as text on startup | **Real** | `src/main.rs`, `generate_default_pin()` + `println!` |
| `--no-pin-auth` flag to disable PIN gate | **Real** | `src/main.rs` `Cli::no_pin_auth` |
| QR code rendering (terminal) | **Real** | `src/main.rs` `print_ticket_qr()` renders the ticket as a unicode-block QR to stdout at startup, before the raw ticket text. `examples/qr_probe.rs` witnesses this: a realistic ~180-char ticket produces a well-formed 53×53 scannable QR. PNG rendering (`--qr-png`) remains designed-only. |
| `Allowlist` struct: load/save/contains/add/remove | **Real, probe-witnessed** (`examples/allowlist_probe.rs`) | `src/allowlist.rs` |
| `verify_pin()` PIN comparison | **Real, probe-witnessed** (`examples/allowlist_probe.rs`) | `src/allowlist.rs` |
| `generate_pin()` / `generate_default_pin()` | **Real, probe-witnessed** (`examples/allowlist_probe.rs`; weak RNG caveat below) | `src/allowlist.rs` |
| PIN + allowlist gate wired into the control-channel accept path | **Real, wired, end-to-end witnessed** (`examples/auth_gate_probe.rs` + a live `iroh` run, see below) | `src/control_channel.rs` `ControlChannel::authenticate`, called from `ProtocolHandler::accept` |
| `ClientMessage::Pin` / `ServerMessage::AuthRejected` wire messages | **Real** | `src/control_channel.rs`, mirrored in `PROTOCOL.md` |
| Device revocation (`remove_entry`) | **Real function, not called from any command/UI** | `src/allowlist.rs::Allowlist::remove_entry` |
| `--rotate-every <duration>` flag | **Real (ticket-reprint on a timer)** | `src/main.rs` `Cli::rotate_every` (parsed by `src/duration.rs::parse_rotate_duration`, accepts `30m`/`2h`/`90s`/`1h30m`) and a `tokio::select!` ticker branch that re-prints the ticket QR and verification phrase each tick via `print_pairing_block()`. `examples/ticket_rotation_probe.rs` witnesses this. **Ticket-string reprint only** — full fresh-keypair identity rotation (which would invalidate old tickets) remains designed-only. See "Open design gap" below. |
| Full PRD P0-2/7.1 spec (mutual short-phrase, Keychain, cross-device revoke) | **Not implemented; separate, larger PRD row** | `holoiroh-pairing-ticket-exchange` |

The PIN-and-allowlist auth gate is **not a design document standing in
for code**. It is real, compiled Rust. Running `cargo run --example
auth_gate_probe` witnesses the real code path directly. This repo uses
runnable `examples/*_probe.rs` witnesses instead of `#[test]` files. The
crate has no unit tests. This work also witnessed the gate end-to-end
over a live `iroh` QUIC connection (see "End-to-end witness" below). The
QR code is real (terminal rendering). `--rotate-every` is now real too,
but only in its **ticket-reprint** form. On each tick, it re-derives and
re-prints the *current* ticket's QR and verification phrase. A stale
screenshot then stops being the one the operator reads off. The larger
**fresh-keypair-per-tick** identity rotation remains designed-only. It
would invalidate old tickets entirely. It requires tearing down and
rebuilding the `iroh` `Live` session mid-run. It is also entangled with
the publish path that only runs past the macOS TCC preflight. See "Open
design gap" below.

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

The ticket string is long: 100+ characters, in iroh's own self-describing,
base32/base64-ish wire format. Copy-pasting it between a Mac terminal and
an iPhone is real friction. This feature's whole pitch is "point your
phone at your Mac and go." A QR code turns "carefully retype or AirDrop a
password-like string" into "open the camera app."

### Chosen crate: `qrcode`

This design uses the [`qrcode`](https://crates.io/crates/qrcode) crate.
This section verified the choice directly against the crates.io API. The
latest published version is `0.14.1`, from July 2024. The crate is
actively maintained and dual-licensed MIT/Apache-2.0. The crate is not yet
added to `Cargo.toml`. This section documents the exact API shape,
verified against the crate's docs.rs page. Implementation is therefore a
mechanical follow-up, not a research task.

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

`render()` is generic over the output "pixel" type. The char-grid form
above, using `light_color`/`dark_color` with `char`, is the
terminal-appropriate instantiation. A denser two-row-per-character
rendering is a known technique for smaller, physical or vertical terminal
QR codes. It uses unicode half-block glyphs (`▀`/`▄`/` `/`█`). These
glyphs pack 2 vertical QR modules into one terminal row, using a
foreground-and-background color per glyph. This pass did not
independently re-verify this technique against the crate's exact API
surface. The plain one-module-per-character form above is the documented,
verified baseline to implement first. The half-block densification is a
follow-up refinement, not a blocker.

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

Terminal rendering is the default. `holoiroh-daemon` normally runs from a
terminal, so this needs no extra window or file to manage. A `--qr-png`
flag opens a Preview window instead, for these cases:

- The terminal's font isn't monospace-square enough to render a clean QR
  (some terminal emulators distort block-character QR codes).
- The operator wants something they can screen-share more reliably than a
  terminal-rendered grid.

### Exact remaining wiring step

None of the above is implemented. To implement:

1. Add `qrcode = "0.14"` to `mac-daemon/Cargo.toml`. By default, this uses
   the crate's `image`/`svg`/`pic` features. Only `image` is needed here.
   The minimal correct spec, once actually added, is
   `qrcode = { version = "0.14", default-features = false, features = ["image"] }`.
2. In `src/main.rs`, construct a `QrCode` from `ticket.to_string()`
   immediately after `println!("{ticket}");`. This line is currently line
   ~140, right before the PIN print added in this pass. Render the QR
   code per the terminal snippet above by default. Behind a new
   `--qr-png` `Cli` flag, render it per the PNG snippet above instead.
3. No changes are needed to `control_channel.rs`, `allowlist.rs`, or the
   wire protocol. This is purely a `main.rs`-local startup-UX addition.

## Ticket rotation policy

### What's real today

`Live::from_env()` (see `main.rs`'s own doc comment and README.md's
"Build status" section) reads `IROH_SECRET` if set. If unset, it
**generates a fresh iroh keypair on every process start**. A fresh keypair
means a fresh node ID and a fresh ticket each time. This means
**rotation-on-restart already happens implicitly today**. It needs no
code change, as long as an operator does not set `IROH_SECRET`.

Setting `IROH_SECRET` is the opposite choice. It pins the daemon to one
stable identity and ticket across restarts. This is useful for a
long-running trusted deployment, where re-pairing on every restart would
be annoying. The allowlist in this pairing scheme gives largely the same
practical benefit at the *device* level, even if the Mac's own ticket
rotates. This works because the daemon recognizes a previously-paired
device by its own node id, not by the Mac's.

### What's implemented: `--rotate-every <duration>` (ticket-reprint form)

The flag is a real CLI arg on `main.rs`'s `Cli`, parsed by the crate's own
dependency-free `src/duration.rs::parse_rotate_duration` (no `humantime`
dependency was added):

```rust
/// Re-print the pairing ticket + verification phrase on a fixed interval
/// while the daemon keeps running (e.g. `30m`, `2h`, `1h30m`), so a stale
/// QR screenshot stops being the one the operator is reading off.
#[arg(long, value_parser = duration::parse_rotate_duration)]
rotate_every: Option<std::time::Duration>,
```

The loop is real too: a `tokio::select!` ticker branch alongside the
existing shutdown wait. On each tick, it re-prints the *current* ticket's
QR and verification phrase via the same `print_pairing_block()` used at
startup. This keeps startup and rotation output byte-identical, apart
from the header label. The first `interval` tick fires immediately. The
loop skips this first tick so the startup print isn't duplicated.
`examples/ticket_rotation_probe.rs` witnesses `parse_rotate_duration`
(`cargo run --example ticket_rotation_probe`). It also witnesses that a
distinct ticket yields a distinct verification phrase via the same
`pairing_phrase()` the reprint calls.

This loop deliberately does **not** mint a fresh `iroh` keypair or
`LiveTicket` per tick. It re-prints the current ticket instead. This is
useful for defeating a stale screenshot and for operator awareness. It
does not invalidate a leaked-but-unused ticket. That larger behavior is
the open gap below.

### Open design gap, stated honestly

Rotating the ticket alone does **not** by itself disconnect a client that
is already connected using the *old* ticket. This is true whether
rotation means generating a new `LiveTicket` from the same running
`Live`/`Endpoint`, or actually tearing down and rebuilding the
`iroh::Endpoint` with a fresh `SecretKey`. `iroh`'s connections are
established per-dial. An already-open `Connection` object does not get
invalidated just because a *new* ticket now points somewhere else, or
nowhere. The exception: if the underlying `Endpoint` itself is torn down
and rebuilt, this would in fact drop existing connections. That is a much
bigger operational change than "print a new ticket string." This means
`--rotate-every` as sketched above would need an explicit decision
between two very different things:

- **Ticket-string rotation only.** This is cheap. It doesn't affect
  connected clients. It is mainly useful to stop a *leaked but
  not-yet-used* ticket from working. This approach requires
  `Live`/`iroh_moq` to support re-publishing a broadcast under a fresh
  identity, without a full `Endpoint` rebuild. This pass did not verify
  whether `iroh-live`'s API supports this without also tearing down and
  losing the currently-connected client's stream.
- **Full identity rotation via `Endpoint` rebuild.** This is expensive. It
  *does* drop existing connections, effectively a scheduled self-restart.
  This is achievable today: run the daemon under a supervisor that
  restarts it every N minutes. This needs no new code at all. However, it
  loses in-flight `holo_bridge` state each time.

This gap is not silently glossed over. **`--rotate-every` is implemented
in its ticket-reprint form** (the cheap, connected-client-safe behavior).
**The full-identity-rotation behavior remains unimplemented**, because it
has an unresolved question about how it should interact with connected
clients and `holo_bridge` state. Settle that question before building the
larger behavior, not partway through it.

### Exact remaining wiring step (for full-identity rotation only)

The flag, the dependency-free duration parser, and the ticker-driven
ticket-reprint loop are done (see above). What remains for the *larger*
fresh-keypair-per-tick behavior:

1. **Resolve the ticket-string-only versus full-identity-rebuild question
   above.** This determines whether the rotation loop calls a (currently
   nonexistent) `Live`/`LocalBroadcast` re-publish method, or tears down
   and rebuilds `live`/`router`/`endpoint` entirely. Either path has
   implications for `holo_bridge`'s already-started subprocess and for
   any in-flight control-channel connection.
2. This rebuild path can only be exercised end-to-end past the macOS TCC
   preflight. The publish path refuses to start without Screen Recording
   and Accessibility grants. This step is gated on the same live daemon
   run as the rest of the streaming feature. See
   `holoiroh-user-action-grant-tcc-and-run-daemon`.

## Auth beyond ticket possession

### The problem

Per README.md's pre-existing "Security model" section, anyone who obtains
the iroh ticket can fully control the Mac via Holo. This includes a
leaked QR screenshot or a shoulder-surfed terminal, among other ways.
Holo's arbitrary computer-use automation is a significant blast radius
for a bare string being the only credential.

### The scheme implemented in this pass

1. **PIN, generated fresh per daemon run, displayed alongside the ticket.**
   `allowlist::generate_default_pin()` produces a 6-digit numeric PIN.
   `main.rs` prints it right after the ticket on every startup, unless
   `--no-pin-auth` is passed. The PIN is never persisted to disk. Only
   the *result* of a successful PIN check (the paired device's id) is
   persisted.

   **Stable-PIN override for dev Macs (`HOLOIROH_PIN`).** Setting the
   `HOLOIROH_PIN` env var makes the daemon use that exact PIN, instead of
   a fresh random one every run.

   The rationale: the iOS app's saved connection profiles (sqlite) store
   the ticket and the PIN. The ticket is already stable across restarts
   (`~/.holoiroh/iroh_secret`). But a per-run random PIN silently
   invalidated the saved PIN on every daemon restart. This was harmless
   for an already-allowlisted device, which skips the PIN gate entirely.
   But it was exactly wrong for a fresh install reconnecting via a saved
   profile.

   `HOLOIROH_PIN` is an env var, not a CLI flag, so the PIN never appears
   in `ps` output.
2. **Persisted device allowlist at `~/.holoiroh/allowlist.json`.**
   `allowlist::Allowlist` is a real struct. `cargo run --example
   allowlist_probe` witnesses its behavior. `load`/`save` round-trip
   JSON. `contains_key`/`add_entry`/`remove_entry` mutate an in-memory
   `Vec<AllowlistEntry>`. Each entry records:
   - `device_id`: the connecting peer's iroh node id, as a hex string
   - an optional `label`
   - a `paired_at` unix timestamp

   A missing file loads as an empty allowlist (the normal first-run
   state). A *corrupt* file is a hard error: it **fails closed**, not
   open. The probe's "corrupt JSON file fails CLOSED" case asserts
   `load(corrupt) -> is_err`.
3. **The accept-path gate, wired for real.**
   `control_channel.rs`'s `ControlChannel::authenticate` (called from
   `ProtocolHandler::accept`, before the "control channel ready" greeting
   is ever sent) runs on every accepted connection:
   - If PIN auth is disabled (`ControlChannel::new`, used when
     `--no-pin-auth` is passed) or the connecting device's id is already
     in the allowlist, the connection proceeds immediately.
   - Otherwise, the daemon expects the *first line* the peer sends to be
     `{"type":"pin","pin":"<candidate>"}` (a new `ClientMessage::Pin`
     variant, additive per `PROTOCOL.md`'s extension policy). The daemon
     rejects anything else first: a `prompt`, malformed JSON, or the peer
     just closing the stream.
   - A correct PIN adds the device to the allowlist and persists it
     immediately. This means the device skips the PIN step on every
     future connection. The daemon checks the PIN via `verify_pin`, an
     XOR-fold comparison that avoids a naive `==`'s early-exit timing
     signal. See that function's doc comment for the precise
     threat-model caveat.
   - A wrong PIN, malformed input, or premature EOF gets a
     `ServerMessage::AuthRejected { text }` reply. The daemon then closes
     the connection via `connection.close(0, b"auth rejected")`. The
     peer never receives the greeting or any bridge functionality.

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
`std::collections::hash_map::RandomState`, rather than a dedicated CSPRNG
crate like `rand::rngs::OsRng`. The standard library re-seeds
`RandomState` from the OS RNG on each call. Per the standard library's
own implementation, this is transitively backed by
`getrandom(2)`/`SecRandomCopyBytes`.

This was a deliberate choice. It avoids adding a new dependency for a
short-lived, single-use, 6-digit pairing PIN. This PIN's entire security
property is "not guessable by someone who doesn't already have the
ticket and isn't actively brute-forcing a live connection." It is not a
long-term cryptographic secret.

If PIN auth is ever extended to something with higher stakes, swap
`RandomState` for `rand::rngs::OsRng` or equivalent. "Higher stakes"
means, for example, a longer-lived credential, or removing the
ticket-possession precondition entirely. This caveat is documented here,
rather than silently assumed adequate forever.

### Device revocation: data structure real, no caller yet

`Allowlist::remove_entry(device_id)` is real code. `allowlist_probe`'s
"remove_entry revokes a previously paired device" case witnesses its
revoke behavior. But **nothing calls it**:

- There is no `--revoke-device <id>` CLI flag.
- There is no control-channel message for revocation.
- There is no kill-switch integration.

`remove_entry` alone also does not force-disconnect a
revoked-but-still-connected device. Removing an allowlist entry does not
touch any already-open `iroh::endpoint::Connection`. This is because the
gate only runs once per new connection, at `accept()` time. Revoking a
device mid-session would need one of two things: an active-connection
registry, which this crate doesn't have yet, or reliance on the daemon's
own kill-switch-and-shutdown as the blunt instrument.

This matches the honesty requirement for this document. The *primitive*
exists, and it is probe-witnessed. The *feature* -- an operator actually
being able to revoke a device -- does not exist.

### End-to-end witness (real, this session)

Beyond the runnable component probes, this session also witnessed the
full pairing lifecycle over a **real, live `iroh` QUIC connection**. The
component probes are `cargo run --example auth_gate_probe`, which
exercises `ControlChannel::authenticate`'s accept/reject decisions
directly, and `cargo run --example control_channel_probe`, which
exercises the wire protocol. This repo witnesses via runnable
`examples/*_probe.rs`, not `#[test]` unit-test files.

The end-to-end witness used the existing `examples/control_probe.rs`,
extended in this pass to speak the new `Pin` message. It ran against a
running `holoiroh-daemon` process, with a stub `holo serve` backend. This
is the same throwaway-stub approach README.md's own "Build status"
section documents using. This setup let the control channel actually
mount:

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

Run C's greeting arrives immediately: no PIN required, no rejection. This
is the allowlist fast-path working correctly for a previously-paired
device.

The ad-hoc test harness used for this witness still asserted the *old*
(pre-pairing) expectation on this specific run. It panicked on its own
assertion. This is a bug in the throwaway witness harness, not in the
daemon. The actual wire response
(`{"type":"status","text":"control channel ready"}`) is exactly the
correct, intended behavior for an allowlisted device.

`examples/control_probe.rs`, as committed to this repo, does not include
the fixed-identity `PROBE_SECRET_HEX` mechanism. This session used that
mechanism to force three runs onto the same device id for this test.
Each normal invocation of the committed example generates a fresh
identity. A fresh identity per invocation is correct for the example's
primary purpose: probing the reject/accept paths independently. The
`PROBE_SECRET_HEX` mechanism was local-only scaffolding for this specific
reconnect-lifecycle witness. It was not committed.

**Rejection (`auth_rejected`), acceptance-via-correct-PIN, and
acceptance-via-prior-allowlist-entry are all three real, witnessed
behaviors of the actual `accept()` code path running against a real
`iroh` connection.** This session witnessed all three by running the
real code path. None of them is designed-only.

## Exact remaining wiring steps, all in one place

For anyone picking this up next:

1. **QR code.** Add the `qrcode` dependency. Render it in `main.rs`,
   right after the ticket `println!`. Render to the terminal by default.
   Use the `--qr-png` flag for Preview.app instead. No control-channel or
   protocol changes are needed. See "QR code UX" above for the exact API
   calls.
2. **`--rotate-every`.** The flag, the duration parser, and the
   ticker-driven ticket-reprint loop are **done** (see "What's
   implemented" above). The only remaining piece is the *full
   fresh-keypair-per-tick* rotation. It still needs two things: the
   ticket-string-only-versus-full-rebuild design question resolved (see
   "Open design gap" above), and the TCC-gated live daemon run needed to
   exercise the `Endpoint` rebuild. This rebuild interacts with
   `holo_bridge`'s subprocess lifecycle and with any in-flight
   control-channel connection. So it is not a drop-in addition.
3. **Device revocation UI.** `Allowlist::remove_entry` exists.
   `allowlist_probe` already witnesses it. Wire it to something an
   operator can actually invoke: a CLI subcommand, a control-channel
   message, or a signal handler. Decide how, if at all, to handle an
   already-connected revoked device.
4. **Full PRD P0-2/7.1 spec.** This covers mutual short-phrase
   verification, iOS Keychain storage, and cross-device revocation
   propagation. This repo tracks it separately, as
   `holoiroh-pairing-ticket-exchange` in the PRD. It is substantially
   larger than the PIN-and-allowlist scheme this document covers: it
   needs iOS-side Keychain integration and a mutual challenge-and-ack
   protocol. It supersedes this document's scheme once built.
