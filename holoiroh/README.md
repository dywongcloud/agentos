# HoloIroh

Remote-view-and-control for a Mac, over a direct P2P connection, driven by
[H Company's Holo3](https://github.com/H-Company-AI) computer-use agent
(`holo-desktop-cli`).

This is a standalone subproject living at `holoiroh/` in this repo. It is
**unrelated** to the rest of the repository (the Next.js/Vercel app) and
does not share code, dependencies, or deployment with it.

## Status

Scaffold only. `mac-daemon` is a skeleton binary crate that builds and
resolves its full dependency graph (including `iroh-live`) but does not yet
capture, publish, or bridge anything. `ios/` is a skeleton SwiftUI app that
builds for iOS 17 but has no pairing, video, or control-channel logic yet.
See "Build status" below for exact, witnessed build results.

## Components

```
holoiroh/
├── Cargo.toml              # Rust workspace manifest (members = ["mac-daemon"])
├── mac-daemon/              # Rust binary crate: the Mac-side daemon
│   ├── Cargo.toml
│   └── src/main.rs
├── ios/                     # Swift Package Manager package: the iOS client
│   ├── Package.swift
│   └── Sources/HoloIrohApp/
│       ├── HoloIrohApp.swift
│       └── ContentView.swift
└── README.md                 # this file
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
   second, bidirectional logical stream (its own `iroh` connection or an
   additional stream multiplexed over the same QUIC connection) carrying
   small structured messages: text prompts and voice transcripts *into*
   the daemon, and status/log/ack events *out* to the iOS app. This is the
   channel the iOS app uses to actually tell Holo what to do and see what
   it's doing.
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
   printed, and the app dials it via the iroh transport (a Swift binding
   / FFI over `iroh`'s Rust core — not yet integrated in this skeleton).
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
Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 30s
```

This resolves and compiles the full transitive dependency graph (`iroh`,
`iroh-live`, `iroh-moq`, `moq-media`, `rusty-capture`, `rusty-codecs`,
platform capture bindings, etc.) and produces a working binary — note the
binary lands in the **workspace root's** `target/debug/holoiroh-daemon`
(not `mac-daemon/target/`), since `mac-daemon` is a workspace member.
Running it prints the skeleton message, confirming the binary is real and
executable, not just compiled.

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

Building with an explicit iOS Simulator target succeeds both ways:

```
$ swift build --sdk "$(xcrun --sdk iphonesimulator --show-sdk-path)" \
    --triple arm64-apple-ios17.0-simulator
Build complete! (4.20s)

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

## Security model (planned, not yet implemented)

Pairing is currently "whoever has the ticket can connect" — the ticket
itself is the credential. Before this is usable beyond local testing it
needs a second factor (PIN entry, or an explicit allow-list keyed by the
connecting peer's iroh node ID) so a leaked ticket alone isn't sufficient,
plus a kill-switch on the Mac side to immediately revoke an active session
and stop the broadcast.

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
