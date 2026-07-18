# HoloIroh

Remote-view-and-control for a Mac, over a direct P2P connection, driven by
[H Company's Holo3](https://github.com/H-Company-AI) computer-use agent
(`holo-desktop-cli`).

This is a standalone subproject living at `holoiroh/` in this repo. It is
**unrelated** to the rest of the repository (the Next.js/Vercel app) and
does not share code, dependencies, or deployment with it.

## Status

`mac-daemon` publishes an `iroh-live` broadcast and ticket with a macOS
ScreenCaptureKit video source attached (`capture.rs`, screen/display
capture only -- no audio yet), selectable via a `--display <index>` CLI
flag (defaults to the primary display), and runs a working bidirectional
control channel (`control_channel.rs`, ALPN `holoiroh/control/1`) bridged
to `holo serve` (`holo_bridge/`) -- see [`PROTOCOL.md`](./PROTOCOL.md) for
the control channel's wire schema. The control channel's accept path now
enforces a PIN + persisted device-allowlist auth gate on unrecognized
devices (real, tested, see [`mac-daemon/PAIRING.md`](./mac-daemon/PAIRING.md))
-- a QR rendering of the ticket and a ticket-rotation flag are designed in
that same doc but not yet implemented. System/mic audio capture is still
not wired up. `ios/` is now a real multi-screen SwiftUI app skeleton that
builds for iOS 17: `ContentView` hosts a `NavigationStack` moving from
`PairingView` (paste an iroh ticket, plus a placeholder "Scan QR" button)
to `MainView` on "connect" (video preview placeholder, prompt text field
+ Send, a placeholder microphone button, and a scrolling status/log list
of `ServerMessage`-equivalent entries). None of this is wired to a real
transport yet -- there is no iroh/FFI networking, no actual QR scanning,
no on-device transcription, and no real video rendering; the log list is
driven by locally-synthesized entries so the UI is demonstrably live
rather than static mock data. The Swift side of the control channel
(actually sending/receiving `PROTOCOL.md`'s JSON over a real connection,
including the `pin`/`auth_rejected` messages) remains unimplemented.
See "Build status" below for exact, witnessed build results.

## Components

```
holoiroh/
├── Cargo.toml                     # Rust workspace manifest (members = ["mac-daemon", "ios-bridge"])
├── PROTOCOL.md                    # control-channel wire schema (ClientMessage/ServerMessage)
├── mac-daemon/                    # Rust binary crate: the Mac-side daemon
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs                # entrypoint: Live + Router + capture + control channel + holo_bridge
│       ├── capture.rs             # macOS ScreenCaptureKit video source (--display <index> selection)
│       ├── control_channel.rs     # iroh ALPN transport for PROTOCOL.md's ClientMessage/ServerMessage
│       └── holo_bridge/           # bridges control messages to `holo serve`'s A2A endpoint
│           ├── mod.rs
│           ├── a2a_client.rs
│           ├── control.rs         # internal ControlMessage/ControlEvent (request_id/context_id-correlated)
│           ├── process.rs         # manages the `holo serve` subprocess
│           └── stop.rs
├── ios-bridge/                    # Rust staticlib crate: extern "C" FFI bridge for iOS
│   ├── Cargo.toml                 # crate-type = ["staticlib", "lib"]
│   └── src/lib.rs                 # ticket-connect/subscribe/poll-next-frame extern "C" fns (stub bodies)
├── ios/                            # Swift Package Manager package: the iOS client
│   ├── Package.swift
│   ├── IROH_FFI.md                 # research: no official Swift bindings for iroh-live -> ios-bridge/ fallback
│   └── Sources/HoloIrohApp/
│       ├── HoloIrohApp.swift       # @main App entry point
│       ├── ContentView.swift       # NavigationStack: PairingView -> MainView
│       ├── PairingView.swift       # paste ticket + Scan QR placeholder + Connect
│       ├── MainView.swift          # video placeholder, prompts, mic, status/log list
│       └── Models/
│           └── ServerMessage.swift # Swift mirror of PROTOCOL.md's wire schema
└── README.md                       # this file
```

## Architecture overview

Two processes, one on each end of a direct peer-to-peer link, plus a
bridge into a third piece of software (`holo-desktop-cli`) that actually
drives the Mac.

```
┌─────────────────────────────┐                      ┌───────────────────────────────┐
│           macOS              │                      │             iOS                │
│                               │                      │                                 │
│  ┌─────────────────────────┐  │                      │  ┌───────────────────────────┐  │
│  │   holoiroh-daemon         │  │                      │  │      HoloIrohApp            │  │
│  │   (mac-daemon/, Rust)      │  │                      │  │      (ios/, SwiftUI)         │  │
│  │                             │  │                      │  │                               │  │
│  │  ScreenCaptureKit ────────┐│  │                      │  │  ┌─────────────────────┐  │  │
│  │  (screen frames)           ││  │                      │  │  │   Pairing screen      │  │  │
│  │                             ││  │   iroh QUIC, P2P,   │  │  │   (scan/paste ticket) │  │  │
│  │  System/mic audio ─────────┤├──┼── NAT hole-punch, ───┼──┼─▶│                       │  │  │
│  │  capture                   ││  │   relay fallback     │  │  ├─────────────────────┤  │  │
│  │                             ││  │                      │  │  │   Live video view      │  │  │
│  │  iroh-live::LocalBroadcast │  │  │                      │  │  │   (renders MoQ/iroh-   │  │  │
│  │  publish() → iroh ticket   │  │  │                      │  │  │    live subscription)  │  │  │
│  │                             │  │                      │  │  └─────────────────────┘  │  │
│  │  ┌───────────────────────┐  │  │                      │  │                               │  │
│  │  │  Control channel        │◀─┼──┼── bidirectional ────┼──┼─▶│  Text prompt input           │  │
│  │  │  (prompts/transcripts   │  │  │   control stream     │  │  │  Voice button (→ transcript) │  │
│  │  │   in, status/log out)   │  │  │                      │  │  │  Status/log panel             │  │
│  │  └───────────┬─────────────┘  │  │                      │  │  └───────────────────────────┘  │  │
│  │               │                │  │                      │  └───────────────────────────────┘  │
│  │               ▼                │  │                      └────────────────────────────────────┘
│  │  ┌───────────────────────┐  │
│  │  │  holo-desktop-cli        │  │
│  │  │  bridge (subprocess or   │  │
│  │  │  IPC to Holo3 agent)     │  │
│  │  │  drives the Mac via      │  │
│  │  │  computer-use actions    │  │
│  │  └───────────────────────┘  │
│  └─────────────────────────┘  │
└─────────────────────────────┘
```

### Mac-side: `mac-daemon` (Rust)

A single long-running process, built on [`iroh-live`](https://github.com/n0-computer/iroh-live)
(n0's real-time audio/video-over-iroh library, itself built on
[`iroh`](https://github.com/n0-computer/iroh) for the P2P QUIC transport
and [MoQ](https://quic.video/) for media framing):

1. **Capture.** Screen frames come from `ScreenCaptureKit` (not the camera
   — this streams the desktop, not a webcam), and audio from the system
   output + optionally the mic, both via `iroh-live`'s capture backends
   (`rusty-capture`/`cpal` under the hood).
2. **Publish.** The captured stream is published as an `iroh-live`
   `LocalBroadcast`, which produces a shareable **iroh ticket** — a
   self-describing string that encodes the daemon's node ID and enough
   routing info for a peer to dial it directly.
3. **Transport.** Connections use `iroh`'s QUIC transport: peers attempt a
   direct connection with NAT hole-punching first, falling back to an iroh
   relay server (n0's or a self-hosted one) if a direct path can't be
   established. This is transparent to the app layer — `iroh-live`
   consumers just see a connected stream.
4. **Control channel.** Alongside the media broadcast, the daemon runs a
   second, bidirectional logical stream carrying small structured JSON
   messages: text prompts and voice transcripts *into* the daemon, and
   status/log/ack events *out* to the iOS app. This is the channel the iOS
   app uses to actually tell Holo what to do and see what it's doing.
   **Implemented** in `mac-daemon/src/control_channel.rs`: a dedicated
   `iroh` ALPN (`holoiroh/control/1`) mounted on the *same*
   `iroh::Endpoint`/`iroh::protocol::Router` as `iroh-live`'s own MoQ/gossip
   protocols (via `Live::register_protocols`, the same composition pattern
   `iroh-live` uses internally for its own two ALPNs) -- same peer, same
   NAT-punch/relay path, same connection lifecycle as the media broadcast,
   which is what "a second logical stream on the same iroh QUIC connection"
   means in `iroh`'s one-`Connection`-per-ALPN model (`iroh` does not
   multiplex distinct app protocols inside a single `Connection` object).
   The wire schema (`ClientMessage`/`ServerMessage`, newline-delimited
   JSON) is specified in [`PROTOCOL.md`](./PROTOCOL.md).
5. **Holo bridge.** Prompts arriving on the control channel are handed to
   `holo-desktop-cli` — [H Company](https://www.hcompany.ai/)'s Holo3
   computer-use agent — which interprets them and drives the Mac (mouse,
   keyboard, app control) to carry out the task. Progress/results are
   relayed back over the control channel so the iOS app's status panel can
   show what Holo is doing in near-real-time, and the screen broadcast
   itself shows the actual visual result on the next frame.

### iOS-side: `HoloIrohApp` (SwiftUI, iOS 17+)

A thin client:

1. **Pairing.** The user pastes or scans (QR) the ticket the Mac daemon
   printed, and the app dials it via the iroh transport. **Neither `iroh`
   nor `iroh-live` ships official Swift bindings for the API this project
   actually needs** (`iroh` has official bindings via the separate
   `n0-computer/iroh-ffi` repo, but that only covers raw `Endpoint`/
   `Connection` — `iroh-live`'s `LocalBroadcast`/`subscribe`/frame-pull
   surface has no bindings at all). See [`ios/IROH_FFI.md`](./ios/IROH_FFI.md)
   for the full research and rationale. The chosen path: a hand-written
   Rust staticlib bridge, [`ios-bridge/`](./ios-bridge) (scaffolded with
   stub `extern "C"` signatures for ticket-connect/subscribe/poll-next-frame;
   real implementations are separate follow-on work), built into an
   `.xcframework` for the Swift side to import — not yet integrated in
   this skeleton.
2. **Live view.** Once connected, the app subscribes to the `iroh-live`
   broadcast and renders incoming video frames plus audio, i.e. a live
   mirror of the Mac's screen.
3. **Prompts.** A text field and a microphone button let the user send
   instructions. Voice input is transcribed (on-device or via a
   transcription service — TBD) before being sent as text over the
   control channel, so the wire format is always a text prompt plus
   metadata, never raw audio.
4. **Status.** A log/status panel surfaces the daemon's control-channel
   events, so the user can see acks, in-progress steps, and completion
   from Holo3 without needing to watch the video feed frame-by-frame.

### Why iroh / iroh-live specifically

- **No signaling server to run.** Ticket-based dialing means the only
  thing that has to be transmitted out-of-band is the ticket string
  itself (paste, QR, airdrop, etc.) — there's no separate account system
  or persistent server the Mac daemon depends on to be reachable.
- **NAT traversal with a safety net.** Direct P2P when possible (LAN,
  favorable NAT), transparent relay fallback when not (symmetric NAT,
  restrictive firewalls) — the app layer doesn't need to know which path
  it got.
- **One transport for both media and control.** `iroh-live` already
  solves the hard "get audio+video across a NAT-punched QUIC connection
  reliably" problem; layering the control channel on the same `iroh`
  endpoint means one connection lifecycle, one reconnect story, instead
  of stitching together two different networking stacks.

## Rust dependency note: `iroh-live` is not on crates.io

As of this writing, `iroh-live` is **not published on crates.io**
(verified directly against the crates.io API — see the comment in
`mac-daemon/Cargo.toml`). `mac-daemon/Cargo.toml` therefore depends on it
via a **git dependency** pinned to a specific commit on the upstream
repo's `main` branch:

```toml
iroh-live = { git = "https://github.com/n0-computer/iroh-live", rev = "5f95758fcd1450e443a9134c9d9342bcc3957b85", package = "iroh-live" }
```

`package = "iroh-live"` is required because the git URL points at the
repo root, which is itself a Cargo workspace (`members = ["iroh-live",
"iroh-live-relay", "iroh-moq", ...]`) — Cargo needs to be told which
workspace member to pull. The pin should be bumped deliberately (not left
to float on `main`), re-verifying the public API hasn't shifted before
moving it.

## Build status (witnessed)

**`cargo build` in `holoiroh/mac-daemon`: succeeds.**

```
Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.96s
```

This resolves and compiles the full transitive dependency graph (`iroh`,
`iroh-live`, `iroh-moq`, `moq-media`, `rusty-capture`, `rusty-codecs`,
platform capture bindings, etc.) and produces a working binary — note the
binary lands in the **workspace root's** `target/debug/holoiroh-daemon`
(not `mac-daemon/target/`), since `mac-daemon` is a workspace member.

**`mac-daemon` now does real `iroh-live` P2P publish work, not just a
skeleton println.** `main.rs` brings up an `iroh-live::Live` session
(`Live::from_env().await?...spawn()`, reading `IROH_SECRET` if set or else
generating a fresh key), registers a `LocalBroadcast` with a macOS
ScreenCaptureKit video source attached (`capture::setup_screen_video` --
`iroh_live::media::capture::ScreenCapturer`, never `CameraCapturer`; see
"Status" above and `capture.rs`'s own doc comment for the exact API calls
and the `--display <index>` display-selection logic), publishes it under
the name `holoiroh`, and prints the resulting `iroh-live:` ticket to
stdout. `holoiroh-daemon --help` shows the new flag:

```
$ ./target/debug/holoiroh-daemon --help
Mac-side holoiroh P2P daemon

Usage: holoiroh-daemon [OPTIONS]

Options:
      --display <DISPLAY>  Which display to capture when multiple are connected, by index into the list `iroh_live::media::capture::ScreenCapturer::list_all()` returns (same ordering `capture::list_displays()` exposes). Omit to use the primary display
  -h, --help               Print help
```

Running the binary and sending it `SIGINT` (`Ctrl-C`) shows this real,
witnessed transcript:

```
$ ./target/debug/holoiroh-daemon
iroh-live:TleiXllmGyIDcEOXtF-AIExJQnPFPlZuzkXmR6OVWNwDAQDAqAFM09EDAQDAqEAB09EDAQDAqP8K09ED/holoiroh
^C
$ echo $?
0
```

The ticket differs on every run because no `IROH_SECRET` is set in this
environment, so `Live::from_env` generates a fresh iroh keypair (and thus
a fresh node ID) each time — setting `IROH_SECRET` pins the daemon to a
stable identity/ticket across restarts. `Ctrl-C` triggers a clean shutdown
(`live.shutdown().await`), exiting `0` rather than aborting ungracefully.

Two build-blocking issues were found and fixed while producing this
witness (both pre-existing in the working tree, not introduced by the
`iroh-live` wiring itself):

- `mac-daemon/Cargo.toml`'s `reqwest` dependency requested the
  `rustls-tls` feature, which was renamed to plain `rustls` as of
  `reqwest` 0.13 — this failed dependency resolution before any code
  compiled. Fixed by using the current feature name.
- The compiled binary failed to *launch* (though it built fine) with
  `dyld: Library not loaded: @rpath/libswift_Concurrency.dylib ...
  Reason: no LC_RPATH's found`, because transitive Apple-platform capture
  dependencies (`moq-media`'s `capture-apple` feature chain) link against
  the system Swift runtime via `@rpath` but this workspace never embedded
  an `LC_RPATH` pointing at it. Fixed by adding `holoiroh/.cargo/config.toml`
  with the same `-Wl,-rpath,/usr/lib/swift` linker flag upstream
  `iroh-live`'s own `.cargo/config.toml` uses for `aarch64-apple-darwin`
  — a separate Cargo workspace (ours) never inherits a git dependency's
  `.cargo/config.toml`, so this has to be duplicated explicitly.

**Control channel (`control_channel.rs` + `holo_bridge/`): `cargo build`
and `cargo test -p holoiroh-daemon` both succeed, including the `[lib]`
target added so `examples/control_probe.rs` can dial the control channel
as an external `iroh` peer.**

```
$ cargo test -p holoiroh-daemon
running 11 tests
test control_channel::tests::client_message_prompt_round_trips ... ok
test control_channel::tests::client_message_voice_transcript_round_trips ... ok
test control_channel::tests::client_message_stop_has_no_text_field ... ok
test control_channel::tests::server_message_ack_omits_null_text ... ok
test control_channel::tests::server_message_status_round_trips_with_text ... ok
test control_channel::tests::server_message_task_progress_round_trips ... ok
test control_channel::tests::server_message_error_round_trips ... ok
test control_channel::tests::malformed_json_is_a_deserialize_error_not_a_panic ... ok
test control_channel::tests::unknown_type_is_a_deserialize_error_not_a_panic ... ok
test control_channel::tests::control_event_ack_maps_to_server_message_ack ... ok
test control_channel::tests::control_event_error_maps_to_server_message_error ... ok

test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

These exercise real `serde_json::to_string`/`from_str` round-trips for
every `ClientMessage`/`ServerMessage` variant against the exact JSON
[`PROTOCOL.md`](./PROTOCOL.md) specifies (optional `text` present/omitted,
malformed/unknown-`type` input producing a `serde_json::Error` rather than
a panic), plus the `ControlEvent` → `ServerMessage` translation.

Running the binary end-to-end was also witnessed, in two configurations:

- **No `holo` CLI on `PATH`** (this repo's sandbox has none):
  `holo_bridge::HoloBridge::start` fails its startup health check, the
  daemon logs a warning and correctly continues *without* mounting the
  control channel (the endpoint still binds, the broadcast still
  publishes, the ticket still prints) — a missing bridge degrades the
  daemon rather than crashing it.
- **Pointed at a stand-in `holo serve`** (a throwaway `/health` +
  agent-card HTTP stub, via `HOLOIROH_HOLO_BIN=<path to stub>`):
  `HoloBridge::start` succeeds (health check + agent-card probe both
  pass) and the control channel **is** mounted. Dialing it from a second
  process (`cargo run --example control_probe -- <ticket>`) against
  `CONTROL_ALPN` reached the daemon's `iroh::protocol::Router`, which
  routed the incoming connection by ALPN to `ControlChannel::accept` —
  witnessed directly in the daemon's own tracing output:
  `router.accept{alpn="holoiroh/control/1"} control channel: accepted
  connection`. Completing the full bidirectional-stream exchange beyond
  that ALPN-dispatch point (the actual `ClientMessage`/`ServerMessage`
  payload) could not be witnessed in this particular sandbox:
  `relay.n0.iroh.link` does not resolve in DNS here and the underlying
  QUIC paths report `HostUnreachable` — general HTTPS egress works, so
  this is specifically `iroh`'s relay/NAT-report infrastructure being
  unreachable from this sandbox, not a defect in `control_channel.rs`.
  The protocol-dispatch layer that *is* reachable without full external
  network access was exercised and confirmed correct.

**`swift build` in `holoiroh/ios`: succeeds, but only when given an iOS
target explicitly.**

Bare `swift build` with no flags **fails** — this is expected, not a bug
in the package. SwiftPM's `swift build` builds for the **host platform**
by default (macOS on this machine), and this package deliberately has no
`.macOS(...)` entry in `Package.swift` (it's an iOS-17+-only package per
spec), so the SwiftUI APIs it uses (`View`, `App`, `Scene`, `@main`, etc.)
aren't available under the default macOS deployment target the toolchain
falls back to. This is the normal, correct failure mode for an iOS-only
SPM package built with the bare CLI command on macOS — it is not evidence
of a defect in `Package.swift` or the Swift sources.

Building with an explicit iOS Simulator target succeeds both ways --
re-witnessed after adding `PairingView.swift`, `MainView.swift`, and
`Models/ServerMessage.swift` (all five source files now compile clean,
no warnings):

```
$ swift build --sdk "$(xcrun --sdk iphonesimulator --show-sdk-path)" \
    --triple arm64-apple-ios17.0-simulator
Build complete! (5.95s)

$ xcodebuild -scheme HoloIrohApp \
    -destination 'generic/platform=iOS Simulator' -sdk iphonesimulator build
** BUILD SUCCEEDED **
```

The `xcodebuild` run compiles both `arm64` and `x86_64` simulator slices
with target triple `arm64-apple-ios17.0-simulator`, confirming the iOS 17
deployment target set in `Package.swift` (`platforms: [.iOS(.v17)]`) is
honored.

In an environment without a full Xcode install (only the Swift open-source
toolchain, no iOS SDKs), neither of the above would be possible — bare
`swift build` failing there would still not indicate a source defect, only
a missing iOS SDK; the correct read is always "does an iOS-targeted build
succeed," not "does the bare host-platform build succeed."

## Security model

**Real, wired, tested as of this writing**: a PIN + device-allowlist second
factor. See [`mac-daemon/PAIRING.md`](./mac-daemon/PAIRING.md) for the full
design and an honest real-vs-designed breakdown (short version: the PIN
generation, allowlist persistence, and the accept-path enforcement gate are
all real code, unit-tested and additionally verified end-to-end over a live
`iroh` connection; a QR-code rendering of the ticket and a `--rotate-every`
rotation flag are designed but not yet implemented; device revocation has a
real, tested data-structure method (`Allowlist::remove_entry`) but no
command/UI wired to call it yet).

In short: the daemon generates a fresh PIN on every startup (printed
alongside the ticket) and only lets an unrecognized device past the control
channel's greeting after it presents that PIN; a device that does so once is
persisted to `~/.holoiroh/allowlist.json` and skips the PIN on future
connections. `--no-pin-auth` reverts to the old ticket-only behavior for
local dev/testing.

**Still missing**: a kill-switch on the Mac side to immediately stop the
broadcast/revoke an *active* session (revocation data exists but nothing
calls it, and even calling it wouldn't drop an already-open connection —
see `PAIRING.md`'s "Device revocation" section), and the fuller mutual
short-phrase-verification + iOS Keychain + cross-device-revocation spec
tracked separately as this repo's `holoiroh-pairing-ticket-exchange` PRD
row (Project Aro PRD P0-2/7.1), which supersedes this PIN+allowlist scheme
once built.

## Setup (once implemented)

**Mac side:**
```
cd holoiroh/mac-daemon
cargo run --release
# prints an iroh ticket; share it with the iOS app
```

**iOS side:** open `holoiroh/ios` in Xcode (or wrap it in a thin
`.xcodeproj`/App target that depends on this package — a pure SPM package
can't itself produce an installable `.app` bundle), build to a simulator
or device, paste/scan the ticket from the Mac, and connect.

## Running as a background service

Running the daemon manually via `cargo run` only lasts as long as that
terminal session — closing the terminal (or logging out) kills it. For
real remote-control use, the Mac side needs to survive terminal-close and
keep running across login sessions. The standard macOS mechanism for this
is a **launchd LaunchAgent**, and one is provided at
[`mac-daemon/LaunchAgent/com.holoiroh.daemon.plist`](./mac-daemon/LaunchAgent/com.holoiroh.daemon.plist).

### Installing the LaunchAgent

1. **Build the release binary first** (the plist points at the release
   build, not `cargo run`'s debug build):
   ```
   cd holoiroh
   cargo build --release
   # binary lands at the WORKSPACE ROOT's target/release/holoiroh-daemon
   # (not mac-daemon/target/) -- see "Build status" above for why.
   ```
2. **Edit the plist's placeholder paths.** launchd plists do **not**
   expand `~` or `$HOME` in `<string>` values, so
   `com.holoiroh.daemon.plist`'s `ProgramArguments`,
   `WorkingDirectory`, `StandardOutPath`, and `StandardErrorPath` entries
   all contain a literal `/Users/YOUR_USERNAME/...` placeholder prefix
   (covering both the repo-checkout path and your home directory, used
   separately for the binary/working-dir paths vs. the `~/Library/Logs`
   paths) that must be replaced with real absolute paths before
   installing. Run this from inside `holoiroh/` (i.e. right after the
   `cd holoiroh` in step 1) — the plist's placeholder already has
   `/holoiroh/...` baked on after the `.../Documents/agentOS` portion,
   so the substitution needs the **parent** of the current directory
   (`cd .. && pwd`), not `pwd` itself, or the result duplicates the
   `holoiroh` path segment. This single command substitutes **both**
   placeholder forms in one pass, since
   `/Users/YOUR_USERNAME/Documents/agentOS` is itself a prefix of
   `/Users/YOUR_USERNAME`:
   ```
   sed -i '' \
     -e "s#/Users/YOUR_USERNAME/Documents/agentOS#$(cd .. && pwd)#g" \
     -e "s#/Users/YOUR_USERNAME#$HOME#g" \
     mac-daemon/LaunchAgent/com.holoiroh.daemon.plist
   ```
   (the first `-e` must run before the second, since it's the more
   specific pattern — reversing the order would let the second rule
   consume the prefix first and leave `$(pwd)/Documents/agentOS`-style
   duplication behind). Or simply open the file and edit the four paths
   by hand — `plutil -lint` (below) will catch a typo either way.
3. **Create the log directory** (launchd creates the log *files* on first
   launch but will not create missing *parent directories* — the load
   silently fails to produce logs if this doesn't exist first):
   ```
   mkdir -p ~/Library/Logs/holoiroh
   ```
4. **Copy the plist into `~/Library/LaunchAgents/`** (this is the
   per-user agent directory — no `sudo` needed, and the agent only runs
   for this user, not system-wide):
   ```
   cp mac-daemon/LaunchAgent/com.holoiroh.daemon.plist ~/Library/LaunchAgents/
   ```
5. **Load it:**
   ```
   launchctl load ~/Library/LaunchAgents/com.holoiroh.daemon.plist
   ```
   `RunAtLoad` is `true`, so this also starts the daemon immediately
   (you don't need to also log out/in to see it running). `KeepAlive` is
   `true`, so launchd relaunches it automatically if it ever exits —
   crash or otherwise — and it will continue to auto-start on every
   subsequent login until unloaded.

### Checking status / logs / stopping

```
# Confirm it's loaded and see its PID / last exit status:
launchctl list | grep com.holoiroh.daemon

# Tail the logs (this is where the iroh ticket gets printed on startup,
# since stdout is redirected here rather than to a terminal):
tail -f ~/Library/Logs/holoiroh/daemon.out.log
tail -f ~/Library/Logs/holoiroh/daemon.err.log

# Stop it (KeepAlive means a plain `kill` gets relaunched immediately —
# unload is the correct way to actually stop it):
launchctl unload ~/Library/LaunchAgents/com.holoiroh.daemon.plist

# Reload after editing the plist or rebuilding the binary:
launchctl unload ~/Library/LaunchAgents/com.holoiroh.daemon.plist
launchctl load ~/Library/LaunchAgents/com.holoiroh.daemon.plist
```

Because the daemon is `KeepAlive`d and prints its iroh ticket to
`daemon.out.log` on every (re)start rather than to an interactive
terminal, pairing from the iOS app means reading the current ticket out
of that log file rather than watching a terminal — the ticket also
changes on every restart unless `IROH_SECRET` is set in
`mac-daemon/.env` to pin a stable node identity (see "Build status"
above).

### iOS distribution: this can't ship to the App Store as-is

`HoloIrohApp` is a remote computer-control client — it lets a phone drive
mouse/keyboard/app actions on a Mac over the network. Apps in this
category face heavy App Review scrutiny (remote-access/remote-control
apps are frequently rejected or pulled for guideline 2.4.5(?) / general
"apps that control other devices" concerns, and an app whose entire
purpose is remote automation of another computer is exactly the shape
Apple's review process is most cautious about). Realistically, this is
**not** an app you submit to the public App Store for this project's
current stage. Two practical alternatives:

**Option A — TestFlight (recommended for beta / sharing with others)**

TestFlight builds still go through **Beta App Review** (a lighter version
of full App Store review, but still real review — not a rubber stamp),
so a remote-control app can still be rejected here too. It's the right
choice when you want to install the app on a device other than the one
plugged into your build machine, or share it with a small group of
testers, without a full public listing.

1. Wrap `holoiroh/ios` (currently a bare SwiftPM package) in an actual
   Xcode App target/`.xcodeproj`, since only an App target — not a raw
   SPM package — can be archived and uploaded. Set the bundle identifier,
   version, and build number in that target.
2. In [App Store Connect](https://appstoreconnect.apple.com), create a
   new app record under your Apple Developer account (requires an active
   $99/yr Apple Developer Program membership) with a matching bundle ID.
3. In Xcode: `Product → Archive`, then in the Organizer window
   `Distribute App → TestFlight & App Store → Upload`.
4. Once processing finishes in App Store Connect, add **internal
   testers** (up to 100, your own team members on the Developer account —
   *no* Beta App Review required for internal-only testing) or **external
   testers** (up to 10,000, invited by email/public link — *does* require
   Beta App Review, typically a much faster/lighter pass than full App
   Store review but can still flag a remote-control app's permissions).
5. Testers install the **TestFlight** app from the App Store, then accept
   your invite link to install `HoloIrohApp` through it. Builds expire
   after 90 days and need re-upload.

**Option B — Direct Xcode device deploy (recommended for personal/solo use)**

This is simplest if you're the only person who will ever run the iOS
app, and it involves **no App Review at all** — Apple's review process
never sees a build you install this way.

1. Connect your iPhone to your Mac via USB (or use wireless debugging
   once paired once over USB: `Xcode → Window → Devices and Simulators`,
   check "Connect via network").
2. Open the wrapped Xcode project for `holoiroh/ios` (see step 1 in
   Option A — you still need an actual App target, not the bare SPM
   package, to run it on a physical device).
3. In the target's `Signing & Capabilities` tab, select your Apple ID
   under `Team` (a free Apple ID works for this — Xcode will auto-create
   a personal-use provisioning profile; a paid Developer account is
   **not** required for this path, only for TestFlight/App Store).
4. Select your physical iPhone as the run destination (top toolbar
   device picker) and hit `Run` (`⌘R`). Xcode builds, signs, and installs
   directly onto the device.
5. On first launch, the phone will show an "Untrusted Developer" prompt —
   go to `Settings → General → VPN & Device Management` on the iPhone and
   trust your developer certificate once.
6. **Caveat:** apps installed this way with a free Apple ID re-sign
   (7-day provisioning profile expiry) — the app stops launching after 7
   days until you reconnect and `Run` from Xcode again. A paid Developer
   Program account extends this to 1 year per build. For truly
   "install once and forget," TestFlight (Option A) or a paid-account
   direct install are the only options that avoid the 7-day free-tier
   expiry.

Either option requires the wrapped App target from step 1 to actually
exist first — see the PRD-tracked row for that in this project's task
list; it is not yet done as of this writing (`holoiroh/ios` is still a
bare `Package.swift`, no `.xcodeproj`).

## NAT traversal and "anywhere in the world" connectivity

This is inherited entirely from `iroh` (via `iroh-live`) and is **not**
custom networking code in this project — the daemon and app just consume
whatever connection `iroh`'s transport layer establishes. Concretely:

1. **Direct P2P first, with automatic hole-punching.** When a peer dials
   an iroh ticket, `iroh` first attempts to establish a **direct** QUIC
   connection between the two machines' public IPs/ports, using standard
   NAT hole-punching techniques (coordinated simultaneous outbound
   packets from both sides, informed by each side's observed
   address/port from iroh's STUN-like address-discovery). When this
   succeeds — which it does for the large majority of home/office/mobile
   networks, including most consumer NAT routers and most cellular
   carrier NAT — traffic flows **directly** between the Mac and the
   phone, with no third-party server in the media/control path at all.
2. **Relay fallback when direct fails.** Some network configurations
   make hole-punching impossible in principle, not just difficult —
   **symmetric NAT** (where the NAT maps each outbound destination to a
   *different* external port, so the port one peer observes isn't the
   port that will actually accept the other peer's return packets) and
   **CGNAT** (carrier-grade NAT, common on cellular networks and some
   ISPs, where many customers share one public IP with no way to open
   inbound ports at all) are the two common real-world cases. When
   direct connection can't be established, `iroh` transparently falls
   back to relaying traffic through one of **iroh's relay servers**
   (n0's hosted relay fleet by default, or a self-hosted relay if
   configured) — the app layer (this daemon, this iOS app, `iroh-live`
   itself) doesn't need to know or care which path it got; it just sees
   a connected stream either way.
3. **What "anywhere in the world" actually means, operationally.** The
   practical claim is: **this works between any two networks that both
   have outbound internet access**, regardless of physical distance —
   home wifi to cellular data, one country to another, corporate network
   to residential ISP, etc. It does *not* require the two devices to be
   on the same LAN, does *not* require port-forwarding or router
   configuration on either end, and does *not* require a static/public IP
   on either side. The one caveat: when a relay is used (rather than a
   direct connection), **latency increases** — traffic now makes an
   extra hop through relay infrastructure instead of going peer-to-peer
   — but **connectivity is preserved**. The relay fallback exists
   specifically so that "one or both sides are behind restrictive NAT"
   degrades to "slightly higher latency," not to "doesn't work at all."
   For a screen-control use case, this means: expect the best (lowest
   latency, most responsive Remote View) case when both ends can
   hole-punch directly, and a still-fully-functional-but-higher-latency
   case when either end is behind symmetric NAT/CGNAT and traffic relays
   — both are "it works," just with different responsiveness.
