# Aro

**Aro** is the product name. The codebase name is `holoiroh`.

Aro gives users remote viewing and control of a Mac through a direct peer-to-peer (P2P) connection.
[H Company's Holo3](https://github.com/H-Company-AI) computer-use agent, `holo-desktop-cli`, drives the Mac.

This standalone subproject is at `holoiroh/` in this repository.
It is unrelated to the Next.js/Vercel app elsewhere in the repository.
The projects do not share code, dependencies, or deployment.

## Status

Aro is an alpha. This repository has a configured `origin`. The current work has been pushed to it.

### Implemented

- The daemon publishes ScreenCaptureKit video through the `iroh-live` media stream.
- The daemon uses hardware H.264 when VideoToolbox is available.
- The control channel uses the same iroh endpoint.
- The control channel requires a PIN and an exact full-ID device allowlist match.
- Each post-authentication envelope has a directional signature from the authenticated iroh endpoint identity.
- Both network boundaries validate signatures, message type, session, expiry, replay, and sequence before dispatch or state mutation.
- `control_channel.rs` creates metadata-only `audit_log` entries on the live task path.
- It also records Tinfoil cloud-egress metadata.
- `holo_bridge/control.rs` uses `sensitive_categories` on the live path.
- Its watchdog can allow access, request consent, or block access before the agent continues.
- The control channel and executor use `task_state` for task lifecycle state.
- The daemon prints the ticket as a terminal Quick Response (QR) code.
- The daemon prints a verification phrase beside the QR code.
- The app derives and displays the same verification phrase before connection.
- The `--rotate-every` option prints the current pairing block again at the selected interval.
- The app implements the control channel, iroh bridge, video path, QR scanner, and speech transcription.
- The app requests on-device transcription when the device and locale support it.
- Tinfoil supports fallback inference, clarification, documents, images, audio, speech, and planning.
- The Tinfoil client verifies enclave attestation before it permits requests.
- The daemon sends the verified ground truth to the app.
- The app displays this evidence in the Verification Center.

### Known gaps

- `capture.rs` captures screen video only.
- It does not capture system audio or microphone audio.
- A launchd service cannot obtain macOS Transparency, Consent, and Control (TCC) permissions.
- Grant Screen Recording and Accessibility in System Settings.
- Some final checks require a physical Mac or iPhone.
- Simulator and headless checks do not replace device-specific verification.

See [`SECURITY.md`](./SECURITY.md) for instruction-channel, confirmation, and egress boundaries.
See [`BUILD.md`](./BUILD.md) for native, iOS, and WebAssembly System Interface (WASI) commands.

## Components

```
holoiroh/
├── Cargo.toml                     # Rust workspace manifest
├── BUILD.md                       # native, iOS, and wasm32-wasip1 build commands
├── PROTOCOL.md                    # control-channel wire schema (ClientMessage/ServerMessage)
├── mac-daemon/                    # Rust binary + lib crate: the Mac-side daemon
│   ├── Cargo.toml
│   ├── PAIRING.md                 # PIN+allowlist design + real-vs-designed status table
│   ├── src/
│   │   ├── main.rs                # entrypoint: auth check + permission preflight + Live + Router + capture + control channel + holo_bridge
│   │   ├── lib.rs                 # library target re-exporting modules for examples/ probes to consume
│   │   ├── capture.rs             # macOS ScreenCaptureKit video source (--display <index> selection)
│   │   ├── control_channel.rs     # iroh ALPN transport for PROTOCOL.md's ClientMessage/ServerMessage + TaskEnvelope + PIN/allowlist accept gate
│   │   ├── allowlist.rs           # persisted device allowlist (~/.holoiroh/allowlist.json) + PIN generation/verification
│   │   ├── auth.rs                # startup check for an existing Holo login token (~/.holo/.env)
│   │   ├── permissions.rs         # macOS Screen Recording + Accessibility TCC preflight
│   │   ├── limits.rs               # PRD 10.4 session/rate-limit constants + helpers (partly enforced -- see "Session & rate limits" below)
│   │   ├── sensitive_categories.rs # PRD §9 class-5 sensitive-app config, used by the live Holo bridge watchdog
│   │   ├── audit_log.rs            # PRD P0-12 metadata-only audit log, written by the live control channel
│   │   ├── task_state.rs          # PRD task lifecycle state machine, used by control-channel and executor paths
│   │   ├── local_model.rs         # manages the loopback llama.cpp subprocess and bounded token configuration
│   │   ├── local_llama_proxy.rs   # constrained loopback OpenAI proxy: hard output cap, cache flag, body/deadline bounds, SSE pass-through
│   │   ├── executor.rs            # PRD 7.3 ComputerUseExecutor trait + HoloDesktopExecutor abstraction seam
│   │   ├── policy.rs              # PRD 7.3/9/P0-7 typed 6-class action taxonomy + decision table; per-action interception remains unavailable in Holo's opaque runtime
│   │   ├── registry.rs           # PRD 8/P0-4 app registry: alias->deterministic-launch routes (~/.holoiroh/registry.*), resolve()->Single/Ambiguous/NotFound, `open -b` launch (plaintext for now; not yet on a live voice path)
│   │   └── holo_bridge/           # bridges control messages to `holo serve`'s A2A endpoint
│   │       ├── mod.rs
│   │       ├── a2a_client.rs
│   │       ├── control.rs         # internal ControlMessage/ControlEvent (request_id/context_id-correlated)
│   │       ├── process.rs         # manages the `holo serve` subprocess (one-time startup health check)
│   │       ├── health.rs          # ongoing health-check loop: polls holo serve, restarts it on crash
│   │       └── stop.rs
│   └── examples/                  # cargo run --example <name>: real-execution probes (no unit tests in this crate)
│       ├── control_probe.rs               # real external iroh dial against a live daemon's control channel
│       ├── control_channel_probe.rs       # ClientMessage/ServerMessage JSON round-trips + ControlEvent mapping
│       ├── envelope_probe.rs              # TaskEnvelope expiry/duplicate/sequence-number rejection rules, in-memory
│       ├── input_request_probe.rs         # input_request/input_response wire types + real timed expiry-to-safe-pause
│       ├── auth_gate_probe.rs             # ControlChannel::authenticate PIN/allowlist gate, in-memory
│       ├── allowlist_probe.rs             # Allowlist load/save/add/remove + PIN generate/verify, real temp files
│       ├── auth_probe.rs                  # auth::extract_api_key / check_holo_token_in against real strings/files
│       ├── permissions_probe.rs           # PreflightResult/MissingPermission construction + instruction text
│       ├── limits_probe.rs                # ActionCounter/SessionTimer/ApprovalToken/clamp_task_runtime, real execution
│       ├── sensitive_categories_probe.rs  # SensitiveCategories load/save/classify + TOML/JSON format inference, real temp files
│       ├── audit_log_probe.rs             # AuditLogger append/round-trip + PRD P0-12 acceptance test (no dictated text on disk)
│       ├── task_state_probe.rs            # TaskState serde round-trips + is_valid_transition, full lifecycle diagram
│       ├── holo_bridge_queue_probe.rs     # HoloControlBridge concurrent-prompt-queueing races
│       ├── holo_stop_probe.rs             # remote kill-switch: ClientMessage::Stop -> ControlMessage::Stop mapping + `holo stop`/`--force` arg construction + a real `holo stop` invocation + handle_stop queue-drain/error paths
│       ├── local_model_probe.rs           # checks llama-server and holo serve argv/env without loading the model
│       ├── local_llama_proxy_probe.rs     # fake-upstream witness for token cap, cache flag, bounds, headers, SSE, and log privacy
│       ├── executor_probe.rs              # every ComputerUseExecutor trait method against the real HoloDesktopExecutor (unreachable A2A backend)
│       ├── policy_probe.rs                # policy 6-class taxonomy + decision table incl. the PRD 16a adversarial zero-send acceptance test
│       └── registry_probe.rs              # registry round-trip + Single/Ambiguous/NotFound resolution + `open -b` deterministic launch
├── ios-bridge/                    # Rust staticlib crate: extern "C" FFI bridge for iOS
│   ├── Cargo.toml                 # crate-type = ["staticlib", "lib"]
│   ├── cbindgen.toml              # cbindgen config for regenerating the C header's type section
│   ├── include/
│   │   ├── HoloirohIosBridge.h    # C header (the Swift-visible surface) matching the extern "C" ABI
│   │   └── module.modulemap       # exposes the header as an importable Swift module
│   ├── src/lib.rs                 # REAL ticket-connect/subscribe/poll-next-frame extern "C" impl (iroh-live subscribe, RGBA8 frames across the C boundary)
│   └── examples/
│       └── ffi_probe.rs           # cargo run --example: exercises the extern "C" construction/error/teardown paths (no unit tests, per repo rule)
├── ios/                            # Swift Package Manager package: the iOS client
│   ├── Package.swift
│   ├── IROH_FFI.md                 # as-built: real subscribe FFI + xcframework packaging + Swift integration (was: research/fallback plan)
│   └── Sources/HoloIrohApp/
│       ├── HoloIrohApp.swift       # @main App entry point
│       ├── ContentView.swift       # NavigationStack: PairingView -> MainView
│       ├── PairingView.swift       # paste ticket + scan QR code + verify phrase + connect
│       ├── MainView.swift          # VideoRenderView, prompts, mic, status/log list
│       ├── VoiceTranscriber.swift  # on-device SFSpeechRecognizer transcription + model wrapper
│       ├── Video/
│       │   ├── VideoFrameSource.swift          # protocol seam: decoded-frame producer (VideoFrame = pixelBuffer | sampleBuffer)
│       │   ├── VideoRenderView.swift           # UIViewRepresentable over AVSampleBufferDisplayLayer + enqueue(CVPixelBuffer/CMSampleBuffer)
│       │   ├── SyntheticVideoFrameSource.swift # on-device animated CVPixelBuffer generator (CADisplayLink) -- render witness, stands in for the network source
│       │   └── IrohLiveFrameSource.swift       # REAL network source: wraps the ios-bridge poll-next-frame FFI into pooled CVPixelBuffers (behind #if canImport(HoloirohIosBridge) so the package still builds unlinked)
│       └── Models/
│           └── ServerMessage.swift # Swift mirror of PROTOCOL.md's wire schema
└── README.md                       # this file
```

## Architecture overview

The architecture has one process at each end of a direct P2P link.
A bridge connects the daemon to `holo-desktop-cli`.
`holo-desktop-cli` drives the Mac.

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
│  │  not captured             ││  │   relay fallback     │  │  ├─────────────────────┤  │  │
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

The daemon is one long-running process.
It uses [`iroh-live`](https://github.com/n0-computer/iroh-live) for real-time media over iroh.
[`iroh`](https://github.com/n0-computer/iroh) provides the P2P QUIC transport.
[Media over QUIC (MoQ)](https://quic.video/) provides media framing.

1. **Capture video.** ScreenCaptureKit supplies desktop frames, not camera frames.
   The daemon does not capture system audio or microphone audio.
2. **Publish video.** The daemon publishes an `iroh-live` `LocalBroadcast`.
   The broadcast creates an iroh ticket.
   The ticket contains the daemon node ID and peer-routing information.
   The daemon uses H.264 video.
   `main.rs` selects `VideoCodec::best_available()`.
   This call selects hardware `VtbH264` when VideoToolbox is available.
   The `iroh-live` default features include `videotoolbox` in this build.
   The daemon uses software openh264 only when no hardware encoder is available.
   This path implements Project Aro product requirements document (PRD) open question OQ-5.
   [`mac-daemon/TRANSPORT_ADR.md`](./mac-daemon/TRANSPORT_ADR.md) records the decision and evidence.
   The existing MoQ path meets the requirement.
   The primary path does not need a custom iroh QUIC video stream or WebRTC.
3. **Transport.** Peers first attempt direct QUIC with network address translation (NAT) hole-punching.
   If this attempt fails, iroh uses an n0-hosted or self-hosted relay.
   This fallback is transparent to the app.
   `iroh-live` consumers see one connected media stream.
4. **Run the control channel.** The daemon also runs a bidirectional logical stream.
   It carries small, structured JavaScript Object Notation (JSON) messages.
   Prompts and voice transcripts travel to the daemon.
   Status, log, and acknowledgement events travel to the app.
   The app uses these messages to direct Holo and show its status.
   `mac-daemon/src/control_channel.rs` implements the control channel.
   It uses the dedicated `holoiroh/control/1` application-layer protocol negotiation (ALPN) identifier.
   The control channel and `iroh-live` use the same `iroh::Endpoint` and `iroh::protocol::Router`.
   `Live::register_protocols` registers the MoQ and gossip protocols.
   The peer, NAT or relay path, and connection lifecycle therefore remain the same.
   Iroh uses one `Connection` per ALPN.
   It does not multiplex distinct application protocols in one `Connection` object.
   [`PROTOCOL.md`](./PROTOCOL.md) specifies newline-delimited JSON `ClientMessage` and `ServerMessage` values.
5. **Bridge to Holo.** The control channel sends prompts to `holo-desktop-cli`.
   [H Company](https://www.hcompany.ai/) provides this Holo3 computer-use agent.
   The agent drives the mouse, keyboard, and apps on the Mac.
   The control channel returns progress and results to the app.
   The app shows these events in its status panel.
   The media stream shows the visual result on the next frame.

### iOS-side: `HoloIrohApp` (SwiftUI, iOS 17+)

The app is a thin client.

1. **Pair.** The user pastes or scans the ticket that the daemon printed.
   The app extracts a ticket from the QR code.
   The app derives a four-word verification phrase from the ticket.
   The app blocks connection until the user confirms that the daemon shows the same phrase.
   The app then dials the ticket through iroh.
   Neither `iroh` nor `iroh-live` provides the required official Swift surface.
   The separate `n0-computer/iroh-ffi` repository covers raw `Endpoint` and `Connection` APIs only.
   It does not cover `iroh-live` `LocalBroadcast`, `subscribe`, or frame pulling.
   [`ios/IROH_FFI.md`](./ios/IROH_FFI.md) records the research and implementation.
   The project uses the handwritten Rust static-library bridge in [`ios-bridge/`](./ios-bridge).
   Its `extern "C"` surface implements ticket connection, subscription, frame polling, and the control channel.
   The bridge builds for the host and `aarch64-apple-ios`.
   The app packages it as an `.xcframework` and links it through the Xcode app target.
2. **Show live video.** `VideoRenderView` uses `AVSampleBufferDisplayLayer`.
   A `VideoFrameSource` supplies decoded frames.
   The bridge pulls decoded RGBA8 frames from the `iroh-live` subscription.
   `IrohLiveFrameSource` wraps these frames in pooled `CVPixelBuffer` values.
   It pushes them into the same render surface.
   `SyntheticVideoFrameSource` remains an on-device render witness for bridge-less builds.
3. **Send prompts.** The user can enter text or use the microphone button.
   The app requests on-device speech transcription when supported.
   The app sends the resulting text and metadata through the control channel.
   The wire format never carries raw audio for prompts.
4. **Show status.** The status panel shows control-channel events.
   These events include acknowledgements, work steps, input requests, and completion.

### Why iroh / iroh-live specifically

- **No signaling server.** The user shares only the ticket out of band.
  Sharing methods include paste, QR code, and AirDrop.
  The daemon does not require a separate account system or persistent signaling server.
- **NAT traversal with relay fallback.** Iroh uses direct P2P when possible.
  Examples include a local area network (LAN) or a favorable NAT.
  It uses a relay for symmetric NAT or restrictive firewalls.
  The app does not select the path.
- **One transport for media and control.** `iroh-live` carries video over NAT-traversed QUIC.
  The control channel uses the same iroh endpoint.
  Both channels therefore share connection and reconnection lifecycles.
  The project does not combine separate networking stacks.

## Rust dependency note: `iroh-live` is not on crates.io

As of this writing, `iroh-live` is **not published on crates.io**. This
fact is verified directly against the crates.io API (see the comment in
`mac-daemon/Cargo.toml`). `mac-daemon/Cargo.toml` therefore depends on it
via a **git dependency**, pinned to a specific commit on the upstream
repo's `main` branch:

```toml
iroh-live = { git = "https://github.com/n0-computer/iroh-live", rev = "5f95758fcd1450e443a9134c9d9342bcc3957b85", package = "iroh-live" }
```

`package = "iroh-live"` is required because the git URL points at the
repo root. That repo root is itself a Cargo workspace (`members =
["iroh-live", "iroh-live-relay", "iroh-moq", ...]`). Cargo needs to be
told which workspace member to pull. Bump the pin deliberately; do not
leave it to float on `main`. Re-verify that the public API did not shift
before moving the pin.

## Build status (witnessed)

**`cargo build --workspace` in `holoiroh/mac-daemon`: succeeds, warning-clean.**

```
Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.92s
```

(This session re-witnessed the build after adding `sensitive_categories.rs`,
`audit_log.rs`, `task_state.rs`, and the `TaskEnvelope`/`input_request`
additions to `control_channel.rs`. `grep -c warning` on a forced rebuild's
output returns `0`.)

This resolves and compiles the full transitive dependency graph (`iroh`,
`iroh-live`, `iroh-moq`, `moq-media`, `rusty-capture`, `rusty-codecs`,
platform capture bindings, etc.). It produces a working binary. Note: the
binary lands in the **workspace root's** `target/debug/holoiroh-daemon`,
not `mac-daemon/target/`, since `mac-daemon` is a workspace member.

**`mac-daemon` now does real `iroh-live` P2P publish work, not just a
skeleton println.** `main.rs` does the following:

1. Loads `IROH_SECRET` when set.
   Otherwise, it loads or creates `~/.holoiroh/iroh_secret`.
   It builds an `iroh::Endpoint` and starts an `iroh-live::Live` session.
   When the Mac lacks an IPv6 default route, it removes the IPv6 transport.
2. Registers a `LocalBroadcast` with a macOS ScreenCaptureKit video source
   attached (`capture::setup_screen_video` -- `iroh_live::media::capture::ScreenCapturer`,
   never `CameraCapturer`). See "Status" above and `capture.rs`'s own doc
   comment for the exact API calls and the `--display <index>`
   display-selection logic.
3. Publishes the broadcast under the name `holoiroh`.
4. Prints the resulting `iroh-live:` ticket to stdout.

`holoiroh-daemon --help` shows the new flag:

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

The transcript above came from an earlier run without a persisted identity.
Current code reads `IROH_SECRET` when it is set.
Otherwise, it loads `~/.holoiroh/iroh_secret` or creates that file with mode `0600`.
The persisted identity keeps the node ID and ticket stable across restarts.
`Ctrl-C` calls `live.shutdown().await` and exits with status `0`.

This session found and fixed two build-blocking issues while producing
this witness. Both issues pre-existed in the working tree; the
`iroh-live` wiring itself did not introduce them:

- `mac-daemon/Cargo.toml`'s `reqwest` dependency requested the
  `rustls-tls` feature. `reqwest` 0.13 renamed this feature to plain
  `rustls`. This mismatch failed dependency resolution before any code
  compiled. The fix uses the current feature name.
- The compiled binary built fine, but failed to *launch*, with this
  error: `dyld: Library not loaded: @rpath/libswift_Concurrency.dylib ...
  Reason: no LC_RPATH's found`. This error happened because transitive
  Apple-platform capture dependencies (`moq-media`'s `capture-apple`
  feature chain) link against the system Swift runtime via `@rpath`. This
  workspace never embedded an `LC_RPATH` pointing at it. The fix adds
  `holoiroh/.cargo/config.toml`, with the same `-Wl,-rpath,/usr/lib/swift`
  linker flag that upstream `iroh-live`'s own `.cargo/config.toml` uses
  for `aarch64-apple-darwin`. A separate Cargo workspace (ours) never
  inherits a git dependency's `.cargo/config.toml`. So this flag has to be
  duplicated explicitly.

**Control channel (`control_channel.rs` + `holo_bridge/`): `cargo build` succeeds.**
The build includes the `[lib]` target.
`examples/control_probe.rs` uses this target.
It dials the control channel as an external iroh peer.
This crate has no `#[cfg(test)]` unit tests.
The repository deliberately removed them.
`cargo test -p holoiroh-daemon` now runs 0 tests.
The repository re-witnessed their coverage with `cargo run --example <name>_probe` binaries:

```
$ cargo test -p holoiroh-daemon
running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo run -p holoiroh-daemon --example control_channel_probe
[... ClientMessage/ServerMessage round-trips for every variant ...]
error: serialize -> {"type":"error","text":"holo-desktop-cli exited unexpectedly (code 1)"}
auth_rejected: serialize -> {"type":"auth_rejected","text":"incorrect PIN"}
=== malformed / unknown input: real deserialize errors, not panics ===
serde_json::from_str("not json") -> is_err=true
serde_json::from_str({"type":"unknown_variant"}) -> is_err=true
=== ServerMessage::from_control_event mapping ===
ControlEvent::Queued{ahead: 2} -> Status { text: Some("queued, 2 ahead") }
control_channel_probe: OK -- all wire-schema cases witnessed via real execution
```

This probe exercises real `serde_json::to_string`/`from_str` round-trips,
for every `ClientMessage`/`ServerMessage` variant, against the exact JSON
[`PROTOCOL.md`](./PROTOCOL.md) specifies. This includes optional `text`
present/omitted, and malformed/unknown-`type` input producing a
`serde_json::Error` rather than a panic. It also includes the
`ControlEvent` → `ServerMessage` translation. `examples/allowlist_probe.rs`
and `examples/permissions_probe.rs` were run the same way, with all cases
passing:

- `examples/allowlist_probe.rs` covers real temp-file round-trips for
  `Allowlist::load`/`save`/`add_entry`/`remove_entry`, PIN
  generation/verification, and the security-relevant "corrupt file fails
  closed, not open" case.
- `examples/permissions_probe.rs` covers `PreflightResult`/`MissingPermission`
  construction and instruction text, including real `stderr` output.

This session also witnessed the binary running end-to-end, in two
configurations:

- **No `holo` CLI on `PATH`** (this repo's sandbox has none):
  `holo_bridge::HoloBridge::start` fails its startup health check. The
  daemon logs a warning, and correctly continues *without* mounting the
  control channel. The endpoint still binds, the broadcast still
  publishes, and the ticket still prints. A missing bridge degrades the
  daemon, rather than crashing it.
- **Pointed at a stand-in `holo serve`** (a throwaway `/health` +
  agent-card HTTP stub, via `HOLOIROH_HOLO_BIN=<path to stub>`):
  `HoloBridge::start` succeeds. The health check and agent-card probe
  both pass, and the control channel **is** mounted. Dialing it from a
  second process (`cargo run --example control_probe -- <ticket>`),
  against `CONTROL_ALPN`, reached the daemon's `iroh::protocol::Router`.
  The router routed the incoming connection by ALPN to
  `ControlChannel::accept`. This is witnessed directly in the daemon's
  own tracing output: `router.accept{alpn="holoiroh/control/1"} control
  channel: accepted connection`. This sandbox could not witness
  completing the full bidirectional-stream exchange beyond that
  ALPN-dispatch point (the actual `ClientMessage`/`ServerMessage`
  payload). `relay.n0.iroh.link` does not resolve in DNS here, and the
  underlying QUIC paths report `HostUnreachable`. General HTTPS egress
  works. So this gap is specifically `iroh`'s relay/NAT-report
  infrastructure being unreachable from this sandbox, not a defect in
  `control_channel.rs`. This session exercised the protocol-dispatch
  layer that *is* reachable without full external network access, and
  confirmed it correct.

The startup authentication and permission preflight is implemented in `auth.rs` and `permissions.rs`.
The `holo serve` health-check loop is implemented in `holo_bridge/health.rs`.
`main.rs` and `HoloBridge` use these paths.
The following probes witness them:

- `examples/auth_probe.rs`
- `examples/auth_gate_probe.rs`
- `examples/permissions_probe.rs`

The probes use real strings, files, or in-memory state.
They cover token parsing, the PIN and allowlist gate, `PreflightResult`, and `MissingPermission`.
All observed cases passed.
This pass did not re-witness the complete daemon path on Mac hardware with both permissions granted.
That path should publish after a successful preflight.
Treat it as implemented but not freshly verified.

**`TaskEnvelope`, `input_request`/`input_response`, `sensitive_categories.rs`,
`audit_log.rs`, `task_state.rs`, and `limits.rs` are each independently
witnessed via their own probe, all passing.**

```
$ cargo run --example envelope_probe
result -> Err(Expired { expires_at: ..., now: ... })
result -> Ok(())  (accepted exactly AT expires_at -- only strictly-after is expired)
replay send -> Err(DuplicateMessageId { message_id: "msg-dup" })
sequence_number=5 again -> Err(SequenceNotMonotonic { got: 5, last_seen: 5 })
sequence_number=3 (regression) -> Err(SequenceNotMonotonic { got: 3, last_seen: 10 })
sequence_number jumps 0 -> 100 -> Ok(())  (gaps allowed)
envelope_probe: OK -- all envelope validation cases witnessed via real execution

$ cargo run --example input_request_probe
[... real serde round-trips for all 5 InputRequestKind variants ...]
OK -- constructed InputRequest carries no credential characters, only metadata
wait_for_expiry resolved after 253.151916ms (requested TTL was 250ms)
OK -- expiry emits ServerMessage::Status (safe pause), never ServerMessage::Error
input_request_probe: OK -- all input_request/input_response wire-schema and real-timed expiry cases witnessed via real execution

$ cargo run --example sensitive_categories_probe
[... real TOML/JSON save/load/classify round-trips ...]
load(corrupt file) -> is_err=true   (fails closed, not silently defaulted)
All sensitive_categories probes passed.
NOTE: this probe only witnesses the data model and file I/O added in this pass. No live policy-interception point exists yet.

$ cargo run --example task_state_probe
[... full 16-flow-state + 4-interactive + 10-terminal lifecycle diagram exercised ...]
ConfidentialAttestationFailed / ConfidentialModelUnavailable : confirmed unreachable inbound and outbound across all 30 states
task_state_probe: OK -- TaskState enum, serde snake_case wire form, and is_valid_transition's full lifecycle diagram witnessed via real execution

$ cargo run --example limits_probe
[... ActionCounter/SessionTimer/ApprovalToken/clamp_task_runtime exercised ...]
limits_probe: OK -- all PRD 10.4 enforcement helpers behaved correctly under real execution.
```

The `sensitive_categories_probe` note records the scope of that historical probe run.
It does not describe the current daemon.
The sensitive-category watchdog now runs on the live Holo bridge path.

`examples/audit_log_probe.rs` exercises the underlying module with real files.
It writes JSON Lines entries and proves append-only behavior.
The PRD P0-12 acceptance check searches on-disk bytes for a dictated-text marker.
The marker is absent.

The probe initially failed its `subdir must not exist yet for this to be a real test` assertion.
A previous run had left the fixed `holoiroh-audit-probe-subdir` directory in `$TMPDIR`.
Every other probe path includes a process identifier (PID) and nanosecond timestamp.
This one path did not.
The defect affected probe temporary-path hygiene, not `audit_log.rs`.
Deleting the stale directory produced a clean run:

```
$ cargo run --example audit_log_probe
=== AuditLogger::new creates the parent directory ===
AuditLogger::new(.../holoiroh-audit-probe-subdir/audit.log) -> parent dir created: true
=== append is true append-only: a fresh AuditLogger on the same path does not truncate ===
after re-opening AuditLogger on the same path and appending once more: 3 line(s)
=== ACCEPTANCE TEST (Project Aro PRD row P0-12): no content ever reaches the audit log ===
log file contains dictated-text marker: false, contains recipient name: false, contains sentence fragment: false
all real metadata fields ARE present (this is not an accidentally-empty-file false pass)
audit_log_probe: OK -- metadata logged, dictated-text content proven absent via real log-file inspection
```

The repository fixed the probe's fixed-directory reuse defect.
The directory name now includes a PID and nanosecond timestamp.
This scheme matches the other probe paths.
The probe is now idempotent.
Two consecutive runs passed without clearing `$TMPDIR`.

This pass also ran `cargo run --example holo_bridge_queue_probe` again.
The probe passed.
It witnesses concurrent prompt queueing against an unreachable agent-to-agent (A2A) endpoint.
The complete daemon and live `holo serve` path remains blocked in this sandbox.
The sandbox lacks an Accessibility TCC grant and a `holo` command on `PATH`.
This condition predates the current changes.
`cargo test -p holoiroh-daemon` still runs 0 tests.

**`swift build` in `holoiroh/ios` succeeds only with an explicit iOS target.**

Bare `swift build` fails because Swift Package Manager (SwiftPM) selects the macOS host by default.
This package intentionally has no `.macOS(...)` entry in `Package.swift`.
It supports iOS 17 and later.
The SwiftUI APIs `View`, `App`, `Scene`, and `@main` are unavailable under the fallback macOS deployment target.
This expected failure does not show a defect in `Package.swift` or the Swift sources.

Building with an explicit iOS Simulator target succeeds both ways. This
pass re-witnessed this after adding `PairingView.swift`, `MainView.swift`,
and `Models/ServerMessage.swift`. All five source files now compile
clean, with no warnings:

```
$ swift build --sdk "$(xcrun --sdk iphonesimulator --show-sdk-path)" \
    --triple arm64-apple-ios17.0-simulator
Build complete! (5.95s)

$ xcodebuild -scheme HoloIrohApp \
    -destination 'generic/platform=iOS Simulator' -sdk iphonesimulator build
** BUILD SUCCEEDED **
```

The `xcodebuild` run compiles both `arm64` and `x86_64` simulator slices,
with target triple `arm64-apple-ios17.0-simulator`. This confirms the iOS
17 deployment target set in `Package.swift` (`platforms: [.iOS(.v17)]`)
is honored.

An environment without a full Xcode install has only the Swift
open-source toolchain, and no iOS SDKs. In that environment, neither of
the above succeeds. A bare `swift build` failure there still does not
indicate a source defect; it indicates only a missing iOS SDK. The
correct read is always "does an iOS-targeted build succeed," not "does
the bare host-platform build succeed."

## Security model

The daemon implements a PIN and exact full-ID device allowlist as a second factor.
[`mac-daemon/PAIRING.md`](./mac-daemon/PAIRING.md) describes the design and implementation status.

- The daemon implements PIN generation, allowlist persistence, and accept-path enforcement.
- Live iroh probes verify this path end to end.
- The daemon prints the ticket as a QR code.
- The daemon prints a four-word verification phrase beside the QR code.
- The app scans the QR code and derives the same phrase from the ticket.
- The app requires user confirmation before it connects.
- The `--rotate-every` option prints the current pairing block again.
- `Allowlist::remove_entry` implements and verifies device revocation in the data structure.
- No command or user interface calls `Allowlist::remove_entry` yet.

By default, the daemon generates a fresh PIN at startup.
Set `HOLOIROH_PIN` to keep the PIN stable across restarts.
The daemon prints the PIN beside the ticket.
An unrecognized device must send this PIN during the control-channel greeting.
After successful authentication, the daemon stores the device in `~/.holoiroh/allowlist.json`.
An allowlisted device skips PIN entry on future connections.
Use `--no-pin-auth` only for local development or testing.
This option restores ticket-only access.

### Remote kill-switch (stop/cancel)

The app provides a Stop or Cancel control.
It stops the agent's current work on the Mac.
The daemon and app implement this path through the control channel.

- **Daemon.** `control_channel::to_control_message` maps `ClientMessage::Stop` to `ControlMessage::Stop`.
  The message can include `context_id`.
  - **Global form:** Current clients omit `context_id`.
    `HoloControlBridge::handle_stop` performs these actions in order:
    1. It cancels the running turn with A2A `tasks/cancel`.
       It uses the daemon's resolved `contextId` and `Task.id`.
       The `a2a_client` `on_ids` callback captures these identifiers during the stream.
       Clients do not have these identifiers.
    2. It drains queued prompts.
       Each queued prompt receives terminal `Done{Canceled}` status.
    3. It discards any paused turn.
    4. It runs the real `holo stop` command from `holo_bridge/stop.rs`.
       This command has the same pause-then-cancel effect as double Escape in `holo-desktop-cli`.
       If the turn remains active after approximately 3 seconds, it runs `holo stop --force`.
  - **Scoped form:** A present `context_id` identifies one turn.
    The daemon sends A2A `tasks/cancel` for that context only.
    It does not drain the queue, run global `holo stop`, or escalate with `--force`.
    It discards a paused turn with the same `context_id`.
    Other paused turns remain.
    If the context is not active, the daemon returns a status message.
    It does not stop an unrelated turn.

  `examples/holo_stop_probe.rs` witnesses these facts:

  - Both forms of the `ClientMessage::Stop` to `ControlMessage::Stop` mapping.
  - Exact argument vectors for `holo stop` and `holo stop --force`.
  - A real, inactive-turn invocation of the installed `~/.holo/bin/holo`.
  - Queue draining in the global path.
  - Cancellation of only the named context in the scoped path.
  - No queue drain or global stop in the scoped path.
  - Survival of a paused turn with a different context.
  - A `ControlEvent::Error` when the `holo` binary is missing.
- **App.** Cancel controls in the `SessionView` panels send `ClientMessage.stop`.
  These panels cover Working, Connecting, Input-needed, and Draft-ready states.
  After the control-channel PIN handshake, `FFIControlChannelSender` writes encoded bytes to the daemon.
  Before the handshake, `LoggingControlChannelSender` performs the same encoding.
  Bridge-less simulator and continuous integration (CI) builds also use this stand-in.
  The status panel shows its output.
  The wire payload is the global `{"type":"stop"}` form.
  Cancel therefore means "stop everything."
  The daemon first cancels the current turn by its resolved identifiers.
  The app has no per-turn identifiers for scoped cancellation.

**Still missing:**

1. The Mac has no control that immediately stops the media stream.
   It also cannot revoke an active, open session.
   Revocation data exists, but no live caller uses it.
   Revoking an allowlist entry does not close an existing connection.
   See the "Device revocation" section in `PAIRING.md`.
2. The broader cross-device revocation specification remains incomplete.
   The `holoiroh-pairing-ticket-exchange` PRD row tracks this work under Project Aro PRD P0-2/7.1.

## Session & rate limits (PRD 10.4)

`mac-daemon/src/limits.rs` is the single source of truth for every
numeric limit Project Aro PRD section 10.4 specifies. Each limit is a
named constant, with a doc comment citing 10.4 directly. As with the rest
of this README, the honest real-vs-designed breakdown matters more than a
bare "implemented" claim. So here is the exact status of each:

| PRD 10.4 limit | Value | Status |
| --- | --- | --- |
| Task request expiry | 30s default | Constant only. `ClientMessage` (`PROTOCOL.md`) carries no timestamp field to compute request age against. This gap needs a wire-schema change (tracked under `holoiroh-task-envelope-protocol`). |
| Active session lifetime | 10min max | Constant + real `SessionTimer` type (independently exercised, see `examples/limits_probe.rs`). Not called from a live call site: no persistent "session" object spans multiple control-channel connections yet. |
| Approval token | 60s TTL, one task + one action | Constant + real `ApprovalToken` type (single-use, TTL-checked, task-scoped; independently exercised). Not wired to a live call site: this codebase has no approval-gating flow yet. |
| Heartbeat | every 5s while active | Constant only. `ClientMessage`/`ServerMessage` have no heartbeat message variant. The natural insertion point is documented as a doc comment on `ControlChannel::accept`'s `tokio::select!`. |
| Disconnect handling | pause after 5s, cancel after 15s unless safely draft-complete | Constants only. `ControlChannel::accept`'s read loop already detects connection loss, but it tears the connection down immediately today. No pause-then-cancel grace period exists, and no "safely draft-complete" task-state concept exists yet (see `holoiroh-task-state-machine-terminal-statuses`). |
| Max active tasks per Mac | 1 | **Really enforced.** This was already the exact behavior of `HoloControlBridge`'s pre-existing `busy`/`queue` mechanism: a second prompt, while one is in flight, is queued, never run concurrently. `limits.rs` now names that behavior explicitly. A `debug_assert!` in `handle_prompt` ties the constant to the `bool`-shaped enforcement it models. |
| Max active controllers per Mac | 1 | **Gap found, honestly reported, not silently fixed.** `ControlChannel::accept` does not reject a second simultaneous connection from an already-allowlisted device. It runs the same accept path independently, and both connections can coexist. Only the most recent sender's connection receives `ControlEvent`s, via the existing `replace_event_sink` reconnect-redirect mechanism. This mechanism was designed for "old connection dropped, new one takes over," not "two connections alive at once." This gap is not wired here, because a real fix changes accept-time rejection behavior for an already-allowlisted device, and needs a product decision on which connection should win. See `limits.rs`'s `MAX_ACTIVE_CONTROLLERS_PER_MAC` doc for the exact code path and the proposed fix shape. |
| Task runtime | 45s default / 120s max | Constants + a real `clamp_task_runtime` function (independently exercised: an over-max request is actually clamped, not passed through). Not wired into `HoloControlBridge::run_prompt`: that function has no per-task deadline/timeout concept today (`send_and_stream` runs to completion with no `tokio::time::timeout` wrapper). |
| Agent action cap | 100 default | **Really enforced.** `ActionCounter` (real, atomic, independently exercised; refuses a 101st `try_record`) is constructed per turn in `HoloControlBridge::run_prompt`. It counts every `TaskUpdate::Working` update. Once the cap is hit, further progress events for that turn are suppressed, and the turn ends with a `ControlEvent::Error` reporting the cap. **Documented limitation:** the cap suppresses and errors the turn client-side, but it does not by itself issue an A2A `tasks/cancel` to halt the agent server-side past the 100th action. A mid-stream abort channel *does* exist today, for the user-driven paths: `a2a_client::cancel`, plus the `on_ids` mid-stream `contextId`/`Task.id` capture, used by `handle_stop` and `cancel_current_turn`. This channel is simply not wired into the counter's own trip; doing so is a small follow-up, not a contract change. |
| Manual input rate | 120 events/s max | Constant only; no channel to attach it to yet. This codebase's wire schema has no `manual_input` message type at all (only `Prompt`/`VoiceTranscript`/`Stop`/`Pin`). The richer 6-stream protocol that PRD 7.1 describes (which includes a dedicated `manual_input` stream) is tracked separately, under `holoiroh-task-envelope-protocol`. |

Verification: `cargo run --example limits_probe` exercises
`ActionCounter`, `SessionTimer`, `ApprovalToken`, and
`clamp_task_runtime` directly, with real execution (not a test file, per
this repo's convention). All four passed, in the witnessed run this
section is based on. `cargo build` in `mac-daemon` stays warning-clean,
with all of the above in place.

## Setup

### Mac

```sh
cd holoiroh/mac-daemon
cargo run --release
# prints an iroh ticket; share it with the app
```

### iOS

1. Open `holoiroh/ios/App/HoloIroh.xcodeproj` in Xcode.
2. Select an iOS Simulator or device.
3. Build and run the app.
4. Paste or scan the ticket from the Mac.
5. Compare the verification phrase with the daemon output.
6. If the phrases match, confirm and connect.

## Running as a background service

A daemon started with `cargo run` ends when its terminal session ends.
Use a launchd LaunchAgent when the daemon must survive terminal closure and user login cycles.
The repository provides [`mac-daemon/LaunchAgent/com.holoiroh.daemon.plist`](./mac-daemon/LaunchAgent/com.holoiroh.daemon.plist).

### Installing the LaunchAgent

1. Build the release binary.
   The property list points to this binary, not the debug binary from `cargo run`.

   ```sh
   cd holoiroh
   cargo build --release
   # binary lands at the workspace root: target/release/holoiroh-daemon
   # it does not land in mac-daemon/target/
   ```

2. Replace the placeholder paths in the property list.
   Launchd does not expand `~` or `$HOME` inside `<string>` values.
   Replace every literal `/Users/YOUR_USERNAME/...` prefix with an absolute path.
   These placeholders occur in `ProgramArguments`, `WorkingDirectory`, `StandardOutPath`, and `StandardErrorPath`.

   Run the following command from `holoiroh/`.
   The first replacement uses the parent directory because the placeholder already includes `/holoiroh/`.
   Keep the replacements in this order.
   The specific pattern must run before the general pattern.

   ```sh
   sed -i '' \
     -e "s#/Users/YOUR_USERNAME/Documents/agentOS#$(cd .. && pwd)#g" \
     -e "s#/Users/YOUR_USERNAME#$HOME#g" \
     mac-daemon/LaunchAgent/com.holoiroh.daemon.plist
   ```

   Reversing the replacements creates `$(pwd)/Documents/agentOS`-style duplication.
   You can edit the four paths manually instead.
   Run `plutil -lint` after either method to detect syntax errors.

3. Create the log directory.
   Launchd creates log files, but it does not create their parent directory.

   ```sh
   mkdir -p ~/Library/Logs/holoiroh
   ```

4. Copy the property list into the per-user LaunchAgents directory.
   Do not use `sudo`.
   The LaunchAgent runs only for the current user.

   ```sh
   cp mac-daemon/LaunchAgent/com.holoiroh.daemon.plist ~/Library/LaunchAgents/
   ```

5. Load the LaunchAgent.

   ```sh
   launchctl load ~/Library/LaunchAgents/com.holoiroh.daemon.plist
   ```

   `RunAtLoad` is `true`, so loading starts the daemon immediately.
   `KeepAlive` is `true`, so launchd restarts the daemon after any exit.
   Launchd also starts it at each later login.
   Run `launchctl unload` to stop this behavior.

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

Because launchd redirects standard output, read the pairing block from `daemon.out.log`.
The daemon prints the ticket, QR code, and verification phrase after each start.
The daemon persists its generated identity in `~/.holoiroh/iroh_secret` with mode `0600`.
This identity keeps the ticket stable across restarts.
If set, `IROH_SECRET` overrides the persisted identity.

### iOS distribution: this can't ship to the App Store as-is

`HoloIrohApp` lets a phone control mouse, keyboard, and app actions on a Mac.
Apple reviews remote-control apps before public App Store distribution.
The applicable guideline number requires verification against Apple's current published guidelines.
Do not guess or record an unverified guideline number.
For the current project stage, use TestFlight or direct Xcode installation instead:

**Option A — TestFlight (recommended for beta / sharing with others)**

TestFlight builds require Beta App Review for external testing.
Internal-only testing does not require Beta App Review.
Use TestFlight when testers cannot install directly from the development Mac.
It also supports distribution to a small tester group without a public listing.
A remote-control app can still fail Beta App Review.

1. Open `holoiroh/ios/App/HoloIroh.xcodeproj`.
   Set the bundle identifier, version, and build number in the app target.
2. Create an app record in [App Store Connect](https://appstoreconnect.apple.com).
   Use the same bundle identifier.
   This path requires an active Apple Developer Program membership at $99 per year.
3. In Xcode, select `Product → Archive`.
   In Organizer, select `Distribute App → TestFlight & App Store → Upload`.
4. After processing finishes, add testers in App Store Connect.
   You can add up to 100 internal testers from your developer team.
   Internal-only testing does not require Beta App Review.
   You can add up to 10,000 external testers by email or public link.
   External testing requires Beta App Review.
   That review can reject the app because of remote-control permissions.
5. Testers install TestFlight from the App Store.
   They accept the invitation and install `HoloIrohApp`.
   Builds expire after 90 days.
   Upload a new build after expiration.

**Option B — Direct Xcode device deploy (recommended for personal/solo use)**

Use direct Xcode installation for personal use.
This method does not use App Review.
Apple does not review a directly installed build.

1. Connect the iPhone to the Mac with Universal Serial Bus (USB).
   For wireless debugging, first pair the device through USB.
   Then open `Xcode → Window → Devices and Simulators`.
   Enable `Connect via network`.
2. Open `holoiroh/ios/App/HoloIroh.xcodeproj`.
   Select its app target before you choose the device.
3. Open the target's `Signing & Capabilities` tab.
   Select your Apple ID under `Team`.
   Xcode creates a personal-use provisioning profile for a free Apple ID.
   This path does not require a paid account.
   TestFlight and App Store distribution require a paid account.
4. Select the physical iPhone as the run destination.
   Select Run or press `⌘R`.
   Xcode builds, signs, and installs the app.
5. On the first launch, the iPhone can show an Untrusted Developer prompt.
   Open `Settings → General → VPN & Device Management`.
   Trust the developer certificate.
6. A free-account provisioning profile expires after 7 days.
   After expiration, reconnect the iPhone and run the app from Xcode again.
   A paid Developer Program account extends direct-install validity to 1 year per build.
   TestFlight or paid-account direct installation avoids the 7-day limit.

Both distribution options use the app target in `holoiroh/ios/App/HoloIroh.xcodeproj`.

## NAT traversal and "anywhere in the world" connectivity

Iroh provides this capability through `iroh-live`.
The project does not implement custom traversal logic.
The daemon and app use the path that iroh establishes.

1. **Attempt direct P2P first.** A peer dials an iroh ticket.
   Iroh first attempts a direct QUIC connection between the public Internet Protocol (IP) addresses and ports.
   It coordinates outbound packets from both peers for NAT hole-punching.
   Iroh discovers each peer's observed address and port through a STUN-like service.
   A successful attempt sends media and control traffic directly between the Mac and iPhone.
   No relay carries that traffic.
2. **Use a relay when direct connection fails.** Symmetric NAT and carrier-grade NAT (CGNAT) can prevent hole-punching.
   Symmetric NAT can assign a different external port for each destination.
   The observed port can therefore reject return packets from the other peer.
   CGNAT lets many customers share one public IP address.
   Customers generally cannot open inbound ports through CGNAT.
   When direct connection fails, iroh uses a relay.
   The default is n0's hosted relay fleet.
   A deployment can configure a self-hosted relay instead.
   The daemon, app, and `iroh-live` do not select or expose this path.
   They receive a connected media stream in either case.
3. **Understand the connectivity boundary.** Both networks must permit outbound internet access.
   Physical distance does not change this requirement.
   Examples include home Wi-Fi to cellular, international connections, and corporate-to-residential connections.

   The connection does not require:

   - Both devices on one LAN.
   - Port forwarding or router configuration.
   - A static or public IP address on either device.

   A relay adds an extra network hop.
   This hop increases latency compared with a direct path.
   Direct hole-punching provides the lowest-latency case.
   Symmetric NAT or CGNAT can force the higher-latency relay case.
   Relay availability and network policy still determine whether fallback connectivity succeeds.

## Inference: hosted default and opt-in Aro Private mode

The daemon uses Holo's configured hosted backend by default.
Set `HOLOIROH_LOCAL_MODEL=1`, `true`, or `yes` to enable local inference.
In local inference mode, `mac-daemon/src/local_model.rs` manages this [`llama.cpp`](https://github.com/ggml-org/llama.cpp) process:

```text
llama-server -hf Hcompany/Holo-3.1-35B-A3B-GGUF:Q4_K_M --host 127.0.0.1 --port 8080
```

- **`-hf …:Q4_K_M`** resolves the downloaded GGUF from this Mac's Hugging Face cache.
  The repository places `mmproj.f16.gguf` beside the weights.
  The `-hf` option loads this vision projector automatically.
  Holo3.1 uses desktop screenshots as vision input.
  The projector is required.
  The daemon does not pass `--no-mmproj`.
- **`--host 127.0.0.1`** binds only to loopback.
  The command builder cannot emit another host.
  Remote systems cannot reach this inference endpoint.
- `llama-server` listens on `http://127.0.0.1:8080`.
  `HOLOIROH_LOCAL_MODEL_PORT` can override this port.
  It must differ from the `holo serve` A2A port.
  `HOLOIROH_HOLO_PORT` controls that port and defaults to `8765`.
- Holo does not receive the direct server URL.
  The daemon starts a second loopback-only proxy on an ephemeral port.
  It gives Holo the proxy's `/v1` URL.
  The proxy forces `cache_prompt: true`.
  It replaces each caller token limit with `n_predict: 512`.
  `HOLOIROH_LOCAL_MAX_TOKENS` accepts values from 1 through 2048.
  The proxy rejects malformed or oversized requests.
  It preserves streamed Server-Sent Events (SSE) responses.

`mac-daemon/src/holo_bridge/process.rs` points `holo serve` at the constrained local proxy.
It passes the proxy URL through `--base-url` and `HAI_AGENT_RUNTIME_BASE_URL`.
Only `HAI_AGENT_RUNTIME_BASE_URL` redirects model inference in `holo-desktop-cli`.
`HAI_BASE_URL` does not perform this function.
The installed source provides this evidence:

- `cli/agent_api.py` maps `--base-url` to `HAI_AGENT_RUNTIME_BASE_URL`.
- `agent_client/launcher.py` sends it to the runtime child.
  It removes `HAI_API_KEY`, which prevents hosted-key disclosure.
- `agent_client/model_gateway.py` uses `HAI_BASE_URL` only for the cloud entitlement-probe gateway region.

On the local path, the daemon also removes `HAI_API_KEY` from the `holo serve` child environment.
The no-cloud property therefore does not depend only on the command's removal logic.

The repository provides executable witnesses for command, environment, and request-rewrite behavior.
`local_model_probe` checks process arguments without loading the model.
`local_llama_proxy_probe` uses a fake loopback upstream.
It checks the token cap, cache flag, body limit, header filtering, SSE streaming, and deadlines.
It also checks that raw prompts are absent.
These probes do not run a live model server.
The GGUF is approximately 21 GB.
It requires substantial memory and several minutes to load.
Running it during every build would waste resources.
[`BENCHMARKS.md`](./BENCHMARKS.md) records the separate end-to-end measurement.
That measurement observed 8.3 seconds per step at 720p on an Apple M3 Pro Mac with 36 GB.

## Aro Confidential Cloud (Tinfoil) — live, attested, and opt-in

Tinfoil support is live when `mac-daemon/.env` contains `TINFOIL_API_KEY`.
Git ignores this environment file.
At startup, the daemon creates one shared, origin-bound `tinfoil-rs` client.
The client verifies the current Sigstore release identity and enclave attestation.
It also verifies the endpoint certificate and attestation-hash binding.
It permits egress only after successful verification.
A 30-second deadline prevents optional attestation from blocking daemon startup.
A verification failure disables Tinfoil.
The daemon does not use an unverified endpoint.

The authenticated control channel sends verified evidence to the app.
This evidence contains the host, code fingerprint, enclave fingerprint, Transport Layer Security (TLS) key, and HPKE key.
It also contains the attestation binding.
The Verification Center displays this evidence for the current session.
The app clears it after disconnection, failure, or reconnection.
Evidence from one Mac therefore cannot remain attached to another session.
The bearer key remains on the Mac.

The shared attested transport serves these paths:

- `tinfoil_proxy.rs`: optional rate-limit fallback to `kimi-k2-6`.
- `clarify.rs`: clarifying-question inference.
- `tinfoil_documents.rs`: document-to-markdown conversion.
- `tinfoil_vision.rs`: image analysis with `gemma4-31b` or `kimi-k2-6`.
  The device redacts text, personally identifiable information (PII), and faces before egress.
  A processing error blocks egress.
- `tinfoil_audio.rs`: optional microphone transcription with `voxtral-small-24b`.
  It also provides speech synthesis with `qwen3-tts`.
  The app does not request a digital system-audio mix.
  Microphone recordings can include nearby ambient or speaker audio.
- `tinfoil_planner.rs`: `glm-5-2` tool calling that returns a reviewable plan.
  Planning does not execute the proposed desktop actions.

Documents, images, recordings, and plans require explicit app actions.
Request identifiers match late responses to the active sheet.
The code bounds control frames, cloud-operation concurrency, recording duration, and recording size.
It also bounds Hypertext Transfer Protocol (HTTP) operations and attestation bodies.
Diagnostics contain bounded metadata and digests.
They do not contain prompts, attachments, images, audio, or transcripts.

This capability replaces the old P0-11 claim that the alpha binary has no cloud-inference path.
The configured hosted Holo backend remains the default.
Local inference remains an explicit opt-in mode.
Tinfoil features require `TINFOIL_API_KEY` and an explicit user action.
Product and compliance documents must describe confidential-inference egress as a current capability.

## Naming: `holoiroh` (technical) vs "Aro" (product)

The directory, Cargo crate, and Swift package use the technical name `holoiroh`.
This name predates the product name.
The Project Aro PRD calls the product Aro.
That PRD is the authoritative product specification.
The product name remains provisional.
The PRD defers trademark, App Store, and domain clearance until public beta.

Keep `holoiroh` for internal technical identifiers.
Use Aro for the app display name and other user-visible text.
Renaming the Cargo workspace and Swift package during development would cause unnecessary changes.
Treat `holoiroh` as the repository, module, or build artifact.
Treat Aro as the installed product.
This scoped naming decision is intentional.
Revisit it at the PRD naming-clearance milestone.

## Contributing note: worktree isolation requires committed files first

Commit target files before you start worktree-isolated agents.
Git worktrees contain tracked content from the selected commit.
A new worktree cannot contain files that exist only as uncommitted changes in the main checkout.
The agent will report those files as missing.

The first scaffold pass exposed this condition.
The `holoiroh/` tree was uncommitted before the first worktree agents started.
Committing the scaffold resolved the failure.

For a new subtree, use one of these methods:

- Commit the scaffold before you create a worktree.
- Use agents without worktree isolation for the scaffold pass.

For later worktree passes, confirm that every target file is committed.
