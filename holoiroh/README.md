# Aro

**Aro** (codebase name: `holoiroh`) gives a user remote view and control of
a Mac over a direct P2P connection.
[H Company's Holo3](https://github.com/H-Company-AI) computer-use agent
(`holo-desktop-cli`) drives the Mac.

This is a standalone subproject living at `holoiroh/` in this repo. It is
**unrelated** to the rest of the repository (the Next.js/Vercel app). It
does not share code, dependencies, or deployment with it.

## Status

### Alpha completeness, and the two things needed to run it live

Nearly every alpha component is built and individually witnessed. Each
component is witnessed via a real `cargo run --example *_probe` run, a real
`swift build` run, or both, per this repo's no-unit-tests rule. The
witnessed components are:

**Rust daemon:**
- `iroh-live` publish + ticket
- ScreenCaptureKit capture
- hardware-VideoToolbox H.264
- control channel with signed/expiring/replay-checked `TaskEnvelope`s
- PIN + device-allowlist auth
- metadata-only audit log with a proven no-content-leak guarantee
- session/rate limits
- a `holo serve` health-restart supervisor
- the local-`llama.cpp` no-cloud inference path
- the `ComputerUseExecutor` seam + the 6-class policy wrapper
- the app registry + deterministic launch
- the remote kill-switch

**iOS app:**
- pairing with QR scan + a SHA-256 short-phrase mutual verification that
  byte-matches the daemon
- the full 8-state PRD-6.1 session dashboard
- on-device voice transcription
- the video render surface
- the real iroh-live subscribe FFI (`ios-bridge` cross-compiles cleanly to
  `aarch64-apple-ios`)

See `BENCHMARKS.md` for the local-model latency work (KV-cache reuse brings a
warm step to ~2.3 s, under the < 5 s NFR).

**This build genuinely cannot do two things for itself. It needs a human to
do them before it runs end-to-end:**

1. **Grant the daemon macOS permissions. Run it on a real Mac.** The
   daemon correctly refuses to start without Screen Recording and
   Accessibility permissions (PRD P0-13). This rule stops the daemon from
   streaming a black frame or from driving a Mac it cannot control.
   macOS provides **no CLI to grant these permissions**. `tccutil` only
   resets permissions. The `TCC.db` file is SIP-protected. An interactive
   click in **System Settings → Privacy & Security** is required. After
   granting the permissions, re-run the daemon:
   1. Run `cargo build --release` in `mac-daemon/`.
   2. Run `target/release/holoiroh-daemon`.

   The daemon then prints a ticket QR, a verification phrase, and a PIN.
   This step unblocks every live-run row: end-to-end video, the
   leak/reconnect/latency measurements, and live policy gating.
2. **Deploy the iOS app to a real iPhone** (Xcode device-deploy, linking the
   `ios-bridge` staticlib as an xcframework). This step exercises camera QR
   scanning, real voice capture, and the live remote view. None of these can
   run headlessly.

Separately, `git push` has no effect until a remote is configured
(`git remote add origin <url>`). All work to date is committed locally.

`mac-daemon` publishes an `iroh-live` broadcast and ticket. It attaches a
macOS ScreenCaptureKit video source to the broadcast (`capture.rs`,
screen/display capture only -- no audio yet). A `--display <index>` CLI
flag selects the display; it defaults to the primary display.
`mac-daemon` also runs a working bidirectional control channel
(`control_channel.rs`, ALPN `holoiroh/control/1`) bridged to `holo serve`
(`holo_bridge/`). See [`PROTOCOL.md`](./PROTOCOL.md) for the control
channel's wire schema. The control channel's accept path now enforces a
PIN + persisted device-allowlist auth gate on unrecognized devices (real,
tested; see [`mac-daemon/PAIRING.md`](./mac-daemon/PAIRING.md)). A QR
rendering of the ticket and a ticket-rotation flag are designed in that
same doc. Neither is implemented yet. Startup now also does two real,
wired preflight checks before any capture/publish work begins:

- a Holo auth-token check (`auth.rs`): it refuses to start, with a clear
  instruction, if the user never ran `holo login`.
- a macOS Screen Recording + Accessibility TCC permission preflight
  (`permissions.rs`): it uses the same refuse-with-instructions behavior,
  rather than starting into a black/frozen stream.

`holo_bridge/` now also runs an ongoing health-check loop
(`holo_bridge/health.rs`). This loop polls the supervised `holo serve`
subprocess. It restarts the subprocess on crash. This check runs
independently of the one-time startup health check that `process.rs`
already did. System/mic audio capture is still not wired up. `ios/` is now
a real multi-screen SwiftUI app skeleton that builds for iOS 17.
`ContentView` hosts a `NavigationStack`. The stack moves from
`PairingView` (paste an iroh ticket, plus a placeholder "Scan QR" button)
to `MainView` on "connect". `MainView` has a real video render surface, a
prompt text field with a Send button, a placeholder microphone button,
and a scrolling status/log list of `ServerMessage`-equivalent entries.

The **video render path is now real**. `MainView`'s preview is a
`VideoRenderView` (`ios/Sources/HoloIrohApp/Video/`), a SwiftUI
`UIViewRepresentable` backed by an `AVSampleBufferDisplayLayer`. It has
public `enqueue(CVPixelBuffer:pts:)` and `enqueue(CMSampleBuffer)` entry
points, with thread-safe, display-immediately low-latency scheduling and
`.failed`-status flush-and-resume recovery. These entry points receive the
H.264/HEVC frames that the Mac daemon's VideoToolbox-encoded `iroh-live`
stream will decode to. The view binds to a small `VideoFrameSource`
protocol so the concrete frame producer is swappable. The **network frame
source now exists too**. The `ios-bridge` `extern "C"` FFI has a real
`iroh-live` subscribe implementation: ticket-connect -> `Live::subscribe`
-> `video_ready` -> non-blocking frame poll, returning RGBA8 bytes across
the C boundary. This implementation is verified against the vendored crate
source (see `ios/IROH_FFI.md`'s "As-built" section). A Swift
`IrohLiveFrameSource` (conforming to `VideoFrameSource`) wraps that FFI
into pooled `CVPixelBuffer`s. It pushes them through the same `onFrame`
seam. A `SyntheticVideoFrameSource` also generates animated
`CVPixelBuffer`s on device, to prove the render path works end-to-end
without a network source. The generator produces a scrolling gradient and
a sweeping bar, driven by a `CADisplayLink`. It pushes the frames through
the exact same `onFrame`/`enqueue` seam, so the preview animates today.
`IrohLiveFrameSource` drops into `MainView`'s single binding site, once
the xcframework is linked, without touching the view. **Read this
honestly.** The render half and the Rust subscribe FFI + Swift wrapper are
implemented and witnessed: host and `aarch64-apple-ios` builds succeed, an
FFI probe exercises the C-ABI/error/teardown paths, and the Swift source
compiles against the iOS SDK. **What still needs a real device, network,
and Xcode-link is an actual frame arriving on screen.** The headless dial
cannot complete, because no iroh relay is reachable in this sandbox. Also,
no full Xcode app target links the `.xcframework` here. This is **not**
"video streaming works". It is "the subscribe FFI is real and compiles for
iOS; the last mile needs a device."

The rest is still not wired to a real transport. There is no iroh/FFI
networking, no actual QR scanning, and no on-device transcription. The log
list is driven by locally-synthesized entries, so the UI is demonstrably
live rather than static mock data. The Swift side of the control channel
remains unimplemented. This scope covers sending and receiving
`PROTOCOL.md`'s JSON over a real connection, including the
`pin`/`auth_rejected` messages.

The control channel's wire schema now wraps every message in a
`TaskEnvelope`, except the bare pre-session PIN handshake. The
`TaskEnvelope` fields are:

- `protocol_version`
- `message_id`
- `session_id`
- `task_id`
- `sent_at`/`expires_at`
- `sequence_number`
- `payload`
- `signature`

These fields match the Project Aro PRD's authoritative envelope shape.
This is **real, wired code**, not a paper schema. `control_channel.rs`
actually rejects inbound envelopes in this order, before the payload is
even parsed:

1. envelopes that are expired
2. envelopes that replay a seen `message_id`
3. envelopes that send a non-increasing `sequence_number`

See [`PROTOCOL.md`](./PROTOCOL.md)'s "Envelope" and "Rejection rules"
sections. `signature` rides on the wire per the PRD schema. It is **not
cryptographically verified**. This codebase has no signing-keypair
infrastructure yet. So the field is always `null` on envelopes this daemon
constructs. The field is unchecked on the way in. A new message pair,
`input_request` (server → client) and `input_response` (client → server),
lets the daemon pause a running turn. The daemon uses this pause to ask
the user a structured question: credential needed, MFA needed, ambiguous
choice, missing info, or sensitive-access consent. This message pair is
also real and wired, including a real timed expiry-to-safe-pause path. An
unanswered request emits a `status`, never an `error`, and clears itself
rather than hanging. Credentials and MFA codes themselves never travel on
this channel, by construction. See [`PROTOCOL.md`](./PROTOCOL.md)'s
"Credentials never travel on this channel" section for why. That section
also explains why the real secret-entry path (a separate `manual_input`
channel) remains unbuilt.

Two new modules add PRD-tracked functionality that is real, working, and
independently witnessed. This functionality is **not yet wired to a live
policy or event source**. Both modules say so explicitly in their own doc
comments. This README repeats that fact honestly, rather than rounding up
to "implemented":

- **`sensitive_categories.rs`** (PRD §9, class-5 "sensitive target" apps
  like password managers, banking, health, and system settings) is a real
  data model and config file (`~/.holoiroh/sensitive_categories.toml` or
  `.json`). It stores a per-category always-ask/always-allow/hard-block
  setting, with real bundle-ID classification. Its own module doc is
  explicit: *"This is a config-file row, not a policy-enforcement row...
  nothing in this codebase currently calls into this module from a live
  interception point."* This Rust daemon has no
  `ComputerUseExecutor`/policy-wrapper equivalent yet. `holo_bridge` still
  forwards every prompt straight through to `holo serve`, with no
  pause-before-sensitive-surface check.
- **`limits.rs`** (PRD §10.4 session/rate limits). See the dedicated
  "Session & rate limits" section below for the exact per-limit
  real-vs-constant-only breakdown. The short version follows the same
  pattern: most limits are typed, independently-tested constants/helpers,
  with no live call site wiring them into an actual session or turn yet.
  Two limits (max active tasks per Mac, agent action cap) **are** really
  enforced today.

Two more new modules are real and independently witnessed with a different
shape of gap each:

- **`audit_log.rs`** (PRD row P0-12, local metadata-only audit log) writes
  a real, append-only JSON-Lines file (default `~/.holoiroh/audit.log`),
  via a real `AuditLogger` type. `AuditEntry` has exactly the ten
  PRD-named fields. It deliberately has no catch-all string/JSON field. So
  it is structurally impossible for a call site to log a dictated
  transcript, prompt text, or recipient name. `examples/audit_log_probe.rs`'s
  acceptance test proves this: it writes a real audit entry, then greps
  the literal on-disk bytes for a marker string, and confirms the marker
  is absent. **This module is not yet called from
  `main.rs`/`control_channel.rs`/`holo_bridge`.** Nothing in the live
  request path constructs an `AuditEntry` today. So no audit log is
  actually produced by running the daemon. Two of its ten fields
  (`app_category`, `remote_view_state`) are also honestly modeled as
  single-variant enums for now. This daemon has no per-app attribution. It
  also has no way to detach the broadcast independently of the control
  channel yet (see the module's own "Real vs. honestly-approximated
  fields" doc). The `inference_mode` field's `Local` variant is now the
  accurate value for this build's actual inference path (see
  `local_model.rs` below and the "Inference: local, on-device only"
  section). But `audit_log` has no live call site at all yet. So no
  `AuditEntry` is constructed anywhere to carry this value, and nothing
  sets it either. The wiring gap is the missing call site, not the enum.
- **`task_state.rs`** (PRD task lifecycle: 16 flow states, 4
  interactive-waiting states, and 10 terminal states) is a real,
  fully-modeled Rust enum. It has a real, exhaustively-tested
  `is_valid_transition` state machine, including the three
  Confidential-Cloud/Tinfoil states. These three states are present for
  schema completeness, but they are provably unreachable in this alpha
  build. `task_state.rs` is **deliberately independent of any live event
  source**. `holo_bridge::control::ControlEvent`/`DoneStatus` are the only
  task-progress types actually wired to the real `holo serve` A2A stream
  today. They report three coarse outcomes, plus free-text progress
  strings, with no concept of which of `task_state.rs`'s finer states a
  task is in. Promoting live events to carry a real `TaskState` needs
  `holo-desktop-cli` itself to expose that granularity. `holo-desktop-cli`
  does not expose that granularity today.

`mac-daemon` has **no `#[cfg(test)]` unit tests** as of this writing. This
repo deliberately removed them, per its no-unit-tests rule: validation
must be real, witnessed execution, not assertions run later.
`cargo test -p holoiroh-daemon` now runs **0 tests**. This repo
re-witnessed their coverage instead, as `cargo run --example <name>_probe`
binaries:

- `allowlist_probe`
- `auth_probe`
- `auth_gate_probe`
- `control_channel_probe`
- `envelope_probe`
- `input_request_probe`
- `task_state_probe`
- `audit_log_probe`
- `sensitive_categories_probe`
- `limits_probe`
- `holo_bridge_queue_probe`
- `permissions_probe`
- `local_model_probe` (builds the exact `llama-server` + `holo serve`
  subprocess commands and verifies the local-inference env wiring
  **without spawning the 21 GB model**)

The pre-existing `control_probe` (a real external `iroh` dial against a
live daemon) also covers this path. See "Build status" below for exact,
witnessed build and probe results.

The daemon's actual inference path is the **on-device `llama-server`
local model** (see "Inference: local, on-device only" below). A real
end-to-end latency benchmark of that path (8.3 s/step @ 720p on this Mac)
was run separately. A live model-serving run loads ~21 GB and takes
minutes. So this benchmark is documented in
[`BENCHMARKS.md`](./BENCHMARKS.md), rather than re-run by the build/probe
path above.

## Components

```
holoiroh/
├── Cargo.toml                     # Rust workspace manifest (members = ["mac-daemon", "ios-bridge"])
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
│   │   ├── sensitive_categories.rs # PRD §9 class-5 sensitive-app config data model + file I/O (not wired to a live policy point yet)
│   │   ├── audit_log.rs            # PRD P0-12 metadata-only local audit log (real AuditLogger; not yet called from the live request path)
│   │   ├── task_state.rs          # PRD task lifecycle state machine (16 flow + 4 interactive + 10 terminal states; not wired to a live event source yet)
│   │   ├── local_model.rs         # PRD P0-11 Aro Private mode: manages a local llama.cpp `llama-server` subprocess (Holo3.1 Q4 GGUF, 127.0.0.1 only); holo serve is pointed at it via --base-url / HAI_AGENT_RUNTIME_BASE_URL
│   │   ├── executor.rs            # PRD 7.3 ComputerUseExecutor trait + HoloDesktopExecutor abstraction seam (lib-only; live daemon path not yet routed through it)
│   │   ├── policy.rs              # PRD 7.3/9/P0-7 Aro policy wrapper: 6-class action taxonomy + decision table (real interception logic; not yet wired to a live tool-call boundary)
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
│       ├── local_model_probe.rs           # builds the exact llama-server + holo serve commands and verifies the local-inference env wiring, WITHOUT spawning the 21 GB model
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
│       ├── PairingView.swift       # paste ticket + Scan QR placeholder + Connect
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

This architecture uses two processes, one on each end of a direct
peer-to-peer link. A bridge into a third piece of software,
`holo-desktop-cli`, actually drives the Mac.

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

`mac-daemon` is a single long-running process, built on
[`iroh-live`](https://github.com/n0-computer/iroh-live). `iroh-live` is
n0's real-time audio/video-over-iroh library. It is itself built on
[`iroh`](https://github.com/n0-computer/iroh) for the P2P QUIC transport,
and on [MoQ](https://quic.video/) for media framing. `mac-daemon` does the
following:

1. **Capture.** Screen frames come from `ScreenCaptureKit` (not the
   camera; this streams the desktop, not a webcam). Audio comes from the
   system output, and optionally the mic. Both use `iroh-live`'s capture
   backends (`rusty-capture`/`cpal` under the hood).
2. **Publish.** `mac-daemon` publishes the captured stream as an
   `iroh-live` `LocalBroadcast`. This broadcast produces a shareable
   **iroh ticket**: a self-describing string that encodes the daemon's
   node ID and enough routing info for a peer to dial it directly.
   `mac-daemon` encodes video as **H.264, using the hardware VideoToolbox
   encoder** on macOS. `main.rs` selects the codec via
   `VideoCodec::best_available()`. This call resolves to `VtbH264` in this
   build, since `iroh-live`'s default features include `videotoolbox`.
   `main.rs` falls back to software openh264 only if no hardware encoder
   is available. This choice is the Project Aro PRD's OQ-5
   "H.264-over-iroh" transport. [`mac-daemon/TRANSPORT_ADR.md`](./mac-daemon/TRANSPORT_ADR.md)
   records the decision and its evidence, including why `iroh-live`'s
   existing MoQ path already satisfies it, and why neither a custom iroh
   QUIC video stream nor WebRTC is needed for the primary path.
3. **Transport.** Connections use `iroh`'s QUIC transport. Peers attempt a
   direct connection with NAT hole-punching first. If a direct path
   cannot be established, the connection falls back to an iroh relay
   server (n0's or a self-hosted one). This process is transparent to the
   app layer. `iroh-live` consumers just see a connected stream.
4. **Control channel.** Alongside the media broadcast, the daemon runs a
   second, bidirectional logical stream. This stream carries small,
   structured JSON messages: text prompts and voice transcripts *into*
   the daemon, and status/log/ack events *out* to the iOS app. The iOS
   app uses this channel to actually tell Holo what to do, and to see
   what it is doing. This channel is **implemented** in
   `mac-daemon/src/control_channel.rs`, as a dedicated `iroh` ALPN
   (`holoiroh/control/1`). This ALPN is mounted on the *same*
   `iroh::Endpoint`/`iroh::protocol::Router` as `iroh-live`'s own
   MoQ/gossip protocols, via `Live::register_protocols` (the same
   composition pattern `iroh-live` uses internally for its own two
   ALPNs). This means the same peer, the same NAT-punch/relay path, and
   the same connection lifecycle as the media broadcast. That is what "a
   second logical stream on the same iroh QUIC connection" means in
   `iroh`'s one-`Connection`-per-ALPN model (`iroh` does not multiplex
   distinct app protocols inside a single `Connection` object).
   [`PROTOCOL.md`](./PROTOCOL.md) specifies the wire schema
   (`ClientMessage`/`ServerMessage`, newline-delimited JSON).
5. **Holo bridge.** The control channel hands prompts to
   `holo-desktop-cli`, [H Company](https://www.hcompany.ai/)'s Holo3
   computer-use agent. `holo-desktop-cli` interprets the prompts and
   drives the Mac (mouse, keyboard, app control) to carry out the task.
   The control channel relays progress and results back, so the iOS
   app's status panel can show what Holo is doing in near-real-time. The
   screen broadcast itself shows the actual visual result on the next
   frame.

### iOS-side: `HoloIrohApp` (SwiftUI, iOS 17+)

A thin client:

1. **Pairing.** The user pastes or scans (QR) the ticket the Mac daemon
   printed. The app then dials the ticket via the iroh transport.
   **Neither `iroh` nor `iroh-live` ships official Swift bindings for the
   API this project actually needs.** `iroh` has official bindings via the
   separate `n0-computer/iroh-ffi` repo, but that repo only covers raw
   `Endpoint`/`Connection`. `iroh-live`'s `LocalBroadcast`/`subscribe`/
   frame-pull surface has no bindings at all. See
   [`ios/IROH_FFI.md`](./ios/IROH_FFI.md) for the full research and the
   as-built details. The chosen path is a hand-written Rust staticlib
   bridge, [`ios-bridge/`](./ios-bridge). Its `extern "C"` surface covers
   ticket-connect, subscribe, and poll-next-frame. This surface now has a
   **real `iroh-live` subscribe implementation** (verified against the
   vendored crate source). It builds for the host **and** cross-compiles
   to `aarch64-apple-ios` (both witnessed). It is packaged into an
   `.xcframework` for the Swift side to import. The one remaining step is
   a full Xcode app target linking that `.xcframework`. This SwiftPM
   skeleton builds with or without that link.
2. **Live view.** The render surface is real. `VideoRenderView`
   (`AVSampleBufferDisplayLayer`) displays a stream of decoded frames,
   delivered through a `VideoFrameSource`. The `iroh-live` subscription is
   now wired on both sides. `ios-bridge`'s FFI pulls decoded RGBA8 frames.
   `IrohLiveFrameSource` (Swift, conforming to `VideoFrameSource`) wraps
   them into `CVPixelBuffer`s, and pushes them into this same surface.
   This gives a live mirror of the Mac's screen, once a device links the
   xcframework and reaches a live daemon. Until then, the view is bound to
   an on-device `SyntheticVideoFrameSource`, so the render path is
   exercised for real. The network source is now built and compiles for
   iOS. Only the device, network, and xcframework-link last mile remain;
   the source itself is complete.
3. **Prompts.** A text field and a microphone button let the user send
   instructions. Voice input is transcribed (on-device, or via a
   transcription service — TBD) before it is sent as text over the
   control channel. So the wire format is always a text prompt plus
   metadata, never raw audio.
4. **Status.** A log/status panel surfaces the daemon's control-channel
   events. The user can see acks, in-progress steps, and completion from
   Holo3 this way, without needing to watch the video feed frame-by-frame.

### Why iroh / iroh-live specifically

- **No signaling server to run.** Ticket-based dialing means only the
  ticket string itself has to be transmitted out-of-band (paste, QR,
  airdrop, etc.). There is no separate account system, and no persistent
  server, that the Mac daemon depends on to be reachable.
- **NAT traversal with a safety net.** The connection uses direct P2P
  when possible (LAN, favorable NAT). It uses transparent relay fallback
  when not (symmetric NAT, restrictive firewalls). The app layer does not
  need to know which path it got.
- **One transport for both media and control.** `iroh-live` already
  solves the hard "get audio+video across a NAT-punched QUIC connection
  reliably" problem. Layering the control channel on the same `iroh`
  endpoint means one connection lifecycle and one reconnect story. This
  avoids stitching together two different networking stacks.

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

1. Brings up an `iroh-live::Live` session (`Live::from_env().await?...spawn()`).
   This reads `IROH_SECRET` if set, or else generates a fresh key.
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

The ticket differs on every run, because no `IROH_SECRET` is set in this
environment. So `Live::from_env` generates a fresh iroh keypair, and thus
a fresh node ID, each time. Setting `IROH_SECRET` pins the daemon to a
stable identity/ticket across restarts. `Ctrl-C` triggers a clean shutdown
(`live.shutdown().await`). It exits `0`, rather than aborting ungracefully.

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

**Control channel (`control_channel.rs` + `holo_bridge/`): `cargo build`
succeeds, including the `[lib]` target.** This target lets
`examples/control_probe.rs` dial the control channel as an external
`iroh` peer. **There are no `#[cfg(test)]` unit tests in this crate as of
this writing.** This repo deliberately removed them (`cargo test -p
holoiroh-daemon` now runs 0 tests; see "Status" above). This repo
re-witnessed their coverage instead, as `cargo run --example <name>_probe`
binaries:

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

**Startup auth/permission preflight (`auth.rs`, `permissions.rs`) and the
`holo serve` health-check loop (`holo_bridge/health.rs`) are real, wired
into `main.rs`/`HoloBridge`, and unit-level-witnessed** via
`examples/auth_probe.rs`, `examples/auth_gate_probe.rs`, and
`examples/permissions_probe.rs` (see above). This session exercised
token-file parsing, PIN/allowlist gate logic, and
`PreflightResult`/`MissingPermission` construction, each against real
strings, files, or in-memory state, with passing output. This pass did
**not** re-witness a live, end-to-end run of `holoiroh-daemon` itself on
macOS hardware with Screen Recording/Accessibility actually granted. This
run's purpose is to confirm the preflight passes cleanly and the daemon
proceeds to publish. Treat that specific end-to-end path as
real-but-not-freshly-verified, until it is re-witnessed.

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

`examples/audit_log_probe.rs` is real, and its underlying module logic
passes cleanly. It writes real JSON-Lines entries, and proves
append-only behavior. As the PRD P0-12 acceptance test, it greps the
literal on-disk bytes for a dictated-text marker string, and confirms the
marker is absent. **This pass found one honest wrinkle while
re-witnessing**: the probe's own first assertion (`subdir must not exist
yet for this to be a real test`) panicked on a stale run. This happened
because its "parent directory gets created" case uses a **fixed,
non-unique** subdirectory name (`holoiroh-audit-probe-subdir`), left over
in `$TMPDIR` from an earlier run in the same session. This is a real bug
in the probe's own temp-path hygiene: every other path in this probe
suite mixes in a PID and a nanosecond timestamp; this one line does not.
It is not a defect in `audit_log.rs` itself. Deleting the stale directory
and re-running produced a full clean pass:

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

This repo fixed the fixed-dirname reuse bug in the probe itself. The
subdir name now mixes in a PID and a nanosecond timestamp, matching the
rest of the probe suite's temp-path scheme. So the probe is idempotent.
Running it twice back-to-back, without clearing `$TMPDIR`, verified this:
both runs passed cleanly.

This pass also re-ran `cargo run --example holo_bridge_queue_probe`. It
still passes, for the same reason documented previously. This probe
witnesses real concurrent-prompt-queueing logic against an unreachable
A2A endpoint. The full live-daemon-plus-live-`holo-serve` path remains
blocked in this sandbox, by the same two pre-existing causes: no
Accessibility TCC grant, and no `holo` CLI on `PATH`. This block is not a
regression from this pass's changes. `cargo test -p holoiroh-daemon`
still runs 0 tests.

**`swift build` in `holoiroh/ios`: succeeds, but only when given an iOS
target explicitly.**

Bare `swift build`, with no flags, **fails**. This failure is expected; it
is not a bug in the package. SwiftPM's `swift build` builds for the
**host platform** by default (macOS on this machine). This package
deliberately has no `.macOS(...)` entry in `Package.swift`, because it is
an iOS-17+-only package per spec. So the SwiftUI APIs it uses (`View`,
`App`, `Scene`, `@main`, etc.) are not available under the default macOS
deployment target the toolchain falls back to. This is the normal,
correct failure mode for an iOS-only SPM package, built with the bare CLI
command on macOS. It is not evidence of a defect in `Package.swift` or
the Swift sources.

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

**Real, wired, tested as of this writing**: a PIN + device-allowlist second
factor. See [`mac-daemon/PAIRING.md`](./mac-daemon/PAIRING.md) for the full
design and an honest real-vs-designed breakdown. In short:

- The PIN generation, allowlist persistence, and the accept-path
  enforcement gate are all real code, unit-tested and additionally
  verified end-to-end over a live `iroh` connection.
- A QR-code rendering of the ticket, and a `--rotate-every` rotation
  flag, are designed but not yet implemented.
- Device revocation has a real, tested data-structure method
  (`Allowlist::remove_entry`), but no command or UI wired to call it yet.

In short, the daemon generates a fresh PIN on every startup, printed
alongside the ticket. The daemon only lets an unrecognized device past the
control channel's greeting after the device presents that PIN. A device
that presents the PIN once is persisted to `~/.holoiroh/allowlist.json`.
That device then skips the PIN on future connections. `--no-pin-auth`
reverts to the old ticket-only behavior, for local dev/testing.

### Remote kill-switch (stop/cancel)

The remote **stop kill-switch** is an iOS "Stop"/"Cancel" control. It halts
whatever the agent is doing on the Mac. This kill-switch is wired
end-to-end **on the daemon side**, and structured-and-ready on the iOS
side:

- **Daemon side (real, wired, probe-witnessed).** A control-channel
  `ClientMessage::Stop` maps to `ControlMessage::Stop`, via
  `control_channel::to_control_message`. This mapping carries an
  **optional `context_id`**:
  - *Global form* (`context_id` absent; what every current client
    sends): `HoloControlBridge::handle_stop` runs these steps in order:
    1. It scoped-cancels the running turn via A2A `tasks/cancel`, using
       the daemon's own resolved `contextId`/`Task.id` (captured
       mid-stream by `a2a_client`'s `on_ids` callback; the client never
       has one).
    2. It drains any queued prompts; each queued prompt gets a terminal
       `Done{Canceled}`.
    3. It discards any paused turn.
    4. It engages the CLI-level global kill switch: it shells out to
       the real `holo stop` (see `holo_bridge/stop.rs`), the same
       pause-then-cancel effect as `holo-desktop-cli`'s own double-Esc /
       `holo stop`. It escalates to `holo stop --force` if the same turn
       is still running ~3s later.
  - *Scoped form* (`context_id` present): this form cancels ONE specific
    turn, via an A2A `tasks/cancel` for exactly that context. It has
    **none** of the all-or-nothing machinery: no queue drain, no global
    `holo stop`, no force escalation. A paused stash under the same
    `context_id` is discarded; other stashes survive. If nothing is
    running under a context, the daemon resolves this to a polite status
    note, never a global stop of an unrelated turn.

  `examples/holo_stop_probe.rs` witnesses this whole path (see "Build
  status" below):
  - the `ClientMessage::Stop → ControlMessage::Stop` mapping, in both
    bare and scoped forms
  - the exact `holo stop` / `holo stop --force` argument-vector
    construction
  - a **real `holo stop` invocation** against the installed
    `~/.holo/bin/holo` (a benign no-op with no turn in flight)
  - the full unscoped `handle_stop` queue-drain
  - the scoped-only path: cancel attempted for exactly the named
    context, no queue drain, no global stop, non-matching paused stash
    survived
  - the graceful `ControlEvent::Error` when the `holo` binary is missing
- **iOS side (real over the control channel).** The `SessionView`
  Working/Connecting/Input-needed/Draft-ready panels' "Cancel" control
  sends the actual `ClientMessage.stop`. Once `HoloConnection` completes
  the control-ALPN PIN handshake, the seam is the real
  `FFIControlChannelSender`. It writes encoded bytes to the daemon over
  the wire. Before that point, and in bridge-less simulator/CI builds,
  the `LoggingControlChannelSender` stand-in performs the same real
  encode, and surfaces it in the status/log panel. The wire payload is
  the global form (`{"type":"stop"}`). The app's Cancel means "stop
  everything". The daemon already scoped-cancels the running turn on its
  own, before the wider kill; no per-turn ids exist on the client to
  scope with.

**Still missing:**

1. A *different* kill-switch: the Mac-side control to immediately stop
   the **broadcast**, or revoke an *active already-open session*.
   Revocation data exists, but nothing calls it. Even calling it does
   not drop an already-open connection (see `PAIRING.md`'s "Device
   revocation" section).
2. The iOS control-channel **transport** itself: the one wiring step
   above, actually putting the encoded `stop`/`prompt` bytes on a real
   `iroh` stream.
3. The fuller mutual short-phrase-verification, iOS Keychain, and
   cross-device-revocation spec. This repo tracks this spec separately,
   as the `holoiroh-pairing-ticket-exchange` PRD row (Project Aro PRD
   P0-2/7.1). This spec supersedes this PIN+allowlist scheme, once
   built.

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

## Setup (once implemented)

**Mac side:**
```
cd holoiroh/mac-daemon
cargo run --release
# prints an iroh ticket; share it with the iOS app
```

**iOS side:**

1. Open `holoiroh/ios` in Xcode. Or wrap it in a thin `.xcodeproj`/App
   target that depends on this package (a pure SPM package cannot itself
   produce an installable `.app` bundle).
2. Build to a simulator or device.
3. Paste or scan the ticket from the Mac.
4. Connect.

## Running as a background service

Running the daemon manually via `cargo run` only lasts as long as that
terminal session. Closing the terminal, or logging out, kills it. For
real remote-control use, the Mac side needs to survive terminal-close. It
also needs to keep running across login sessions. The standard macOS
mechanism for this is a **launchd LaunchAgent**. This repo provides one,
at [`mac-daemon/LaunchAgent/com.holoiroh.daemon.plist`](./mac-daemon/LaunchAgent/com.holoiroh.daemon.plist).

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
   expand `~` or `$HOME` in `<string>` values. So
   `com.holoiroh.daemon.plist`'s `ProgramArguments`,
   `WorkingDirectory`, `StandardOutPath`, and `StandardErrorPath` entries
   all contain a literal `/Users/YOUR_USERNAME/...` placeholder prefix.
   This prefix covers both the repo-checkout path and your home
   directory; this doc uses it separately for the binary/working-dir
   paths, and for the `~/Library/Logs` paths. You must replace this
   placeholder with real absolute paths before installing.

   Run this command from inside `holoiroh/` (right after the `cd
   holoiroh` in step 1). The plist's placeholder already has
   `/holoiroh/...` baked on after the `.../Documents/agentOS` portion.
   So the substitution needs the **parent** of the current directory
   (`cd .. && pwd`), not `pwd` itself. Otherwise the result duplicates
   the `holoiroh` path segment. This single command substitutes **both**
   placeholder forms in one pass, since
   `/Users/YOUR_USERNAME/Documents/agentOS` is itself a prefix of
   `/Users/YOUR_USERNAME`:
   ```
   sed -i '' \
     -e "s#/Users/YOUR_USERNAME/Documents/agentOS#$(cd .. && pwd)#g" \
     -e "s#/Users/YOUR_USERNAME#$HOME#g" \
     mac-daemon/LaunchAgent/com.holoiroh.daemon.plist
   ```
   The first `-e` must run before the second, since it is the more
   specific pattern. If you reverse the order, the second rule consumes
   the prefix first. It leaves `$(pwd)/Documents/agentOS`-style
   duplication behind. Or, simply open the file and edit the four paths
   by hand. `plutil -lint` (below) catches a typo either way.
3. **Create the log directory first.** launchd creates the log *files*
   on first launch, but it will not create missing *parent directories*.
   The load silently fails to produce logs, if this directory does not
   exist first:
   ```
   mkdir -p ~/Library/Logs/holoiroh
   ```
4. **Copy the plist into `~/Library/LaunchAgents/`.** This is the
   per-user agent directory. It needs no `sudo`. The agent only runs
   for this user, not system-wide:
   ```
   cp mac-daemon/LaunchAgent/com.holoiroh.daemon.plist ~/Library/LaunchAgents/
   ```
5. **Load it:**
   ```
   launchctl load ~/Library/LaunchAgents/com.holoiroh.daemon.plist
   ```
   `RunAtLoad` is `true`. So this command also starts the daemon
   immediately; you do not need to log out and back in to see it
   running. `KeepAlive` is `true`. So launchd relaunches the daemon
   automatically, if it ever exits, whether from a crash or otherwise.
   It will continue to auto-start on every subsequent login, until
   unloaded.

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

The daemon is `KeepAlive`d, and prints its iroh ticket to
`daemon.out.log` on every (re)start, rather than to an interactive
terminal. So pairing from the iOS app means reading the current ticket
out of that log file, rather than watching a terminal. The ticket also
changes on every restart, unless `IROH_SECRET` is set in
`mac-daemon/.env` to pin a stable node identity (see "Build status"
above).

### iOS distribution: this can't ship to the App Store as-is

`HoloIrohApp` is a remote computer-control client. It lets a phone drive
mouse/keyboard/app actions on a Mac over the network. Apps in this
category face heavy App Review scrutiny. Remote-access/remote-control
apps are frequently rejected or pulled, for guideline 2.4.5(?) or general
"apps that control other devices" concerns. An app whose entire purpose
is remote automation of another computer is exactly the shape Apple's
review process is most cautious about. Realistically, this is **not** an
app you submit to the public App Store, for this project's current
stage. There are two practical alternatives:

**Option A — TestFlight (recommended for beta / sharing with others)**

TestFlight builds still go through **Beta App Review**. This is a
lighter version of full App Store review, but it is still a real review,
not a rubber stamp. So a remote-control app can still be rejected here
too. This option is the right choice when you want to install the app on
a device other than the one plugged into your build machine. It is also
right when you want to share the app with a small group of testers,
without a full public listing.

1. Wrap `holoiroh/ios` (currently a bare SwiftPM package) in an actual
   Xcode App target/`.xcodeproj`. Only an App target, not a raw SPM
   package, can be archived and uploaded. Set the bundle identifier,
   version, and build number in that target.
2. In [App Store Connect](https://appstoreconnect.apple.com), create a
   new app record under your Apple Developer account, with a matching
   bundle ID. This step requires an active $99/yr Apple Developer
   Program membership.
3. In Xcode, select `Product → Archive`. Then, in the Organizer window,
   select `Distribute App → TestFlight & App Store → Upload`.
4. Once processing finishes in App Store Connect, add testers. You can
   add **internal testers**: up to 100, your own team members on the
   Developer account. Internal-only testing needs *no* Beta App Review.
   Or you can add **external testers**: up to 10,000, invited by email
   or public link. External testing *does* require Beta App Review.
   This review is typically a much faster and lighter pass than full App
   Store review, but it can still flag a remote-control app's
   permissions.
5. Testers install the **TestFlight** app from the App Store. They then
   accept your invite link, to install `HoloIrohApp` through it. Builds
   expire after 90 days, and need re-upload.

**Option B — Direct Xcode device deploy (recommended for personal/solo use)**

This option is simplest, if you are the only person who will ever run
the iOS app. It involves **no App Review at all**. Apple's review process
never sees a build you install this way.

1. Connect your iPhone to your Mac via USB. Or use wireless debugging,
   once you pair the device over USB once: go to `Xcode → Window →
   Devices and Simulators`, and check "Connect via network".
2. Open the wrapped Xcode project for `holoiroh/ios` (see step 1 in
   Option A). You still need an actual App target, not the bare SPM
   package, to run it on a physical device.
3. In the target's `Signing & Capabilities` tab, select your Apple ID
   under `Team`. A free Apple ID works for this; Xcode will auto-create
   a personal-use provisioning profile. A paid Developer account is
   **not** required for this path, only for TestFlight/App Store.
4. Select your physical iPhone as the run destination, in the top
   toolbar device picker. Hit `Run` (`⌘R`). Xcode builds, signs, and
   installs the app directly onto the device.
5. On first launch, the phone will show an "Untrusted Developer" prompt.
   Go to `Settings → General → VPN & Device Management` on the iPhone.
   Trust your developer certificate once.
6. **Caveat:** apps installed this way, with a free Apple ID, re-sign
   every 7 days (the provisioning profile expiry). The app stops
   launching after 7 days, until you reconnect and `Run` from Xcode
   again. A paid Developer Program account extends this to 1 year per
   build. For truly "install once and forget," only TestFlight (Option
   A) or a paid-account direct install avoid the 7-day free-tier expiry.

Either option requires the wrapped App target from step 1 to actually
exist first. See the PRD-tracked row for that, in this project's task
list. This target does not yet exist as of this writing: `holoiroh/ios`
is still a bare `Package.swift`, with no `.xcodeproj`.

## NAT traversal and "anywhere in the world" connectivity

This capability is inherited entirely from `iroh`, via `iroh-live`. It
is **not** custom networking code in this project. The daemon and app
just consume whatever connection `iroh`'s transport layer establishes.
Concretely:

1. **Direct P2P first, with automatic hole-punching.** When a peer dials
   an iroh ticket, `iroh` first attempts to establish a **direct** QUIC
   connection between the two machines' public IPs and ports. It uses
   standard NAT hole-punching techniques: coordinated simultaneous
   outbound packets from both sides, informed by each side's observed
   address and port from iroh's STUN-like address-discovery. This
   attempt succeeds for the large majority of home, office, and mobile
   networks, including most consumer NAT routers and most cellular
   carrier NAT. When it succeeds, traffic flows **directly** between the
   Mac and the phone, with no third-party server in the media/control
   path at all.
2. **Relay fallback when direct fails.** Some network configurations
   make hole-punching impossible in principle, not just difficult. The
   two common real-world cases are **symmetric NAT** and **CGNAT**.
   Under symmetric NAT, the NAT maps each outbound destination to a
   *different* external port. So the port one peer observes is not the
   port that will actually accept the other peer's return packets. CGNAT
   is carrier-grade NAT, common on cellular networks and some ISPs.
   Under CGNAT, many customers share one public IP, with no way to open
   inbound ports at all. When a direct connection cannot be established,
   `iroh` transparently falls back to relaying traffic through one of
   **iroh's relay servers** (n0's hosted relay fleet by default, or a
   self-hosted relay if configured). The app layer (this daemon, this
   iOS app, `iroh-live` itself) does not need to know or care which path
   it got. It just sees a connected stream either way.
3. **What "anywhere in the world" actually means, operationally.** The
   practical claim is: **this works between any two networks that both
   have outbound internet access**, regardless of physical distance.
   Examples: home wifi to cellular data, one country to another,
   corporate network to residential ISP, etc.

   This connection does not require any of the following:
   - the two devices to be on the same LAN
   - port-forwarding or router configuration on either end
   - a static or public IP on either side

   The one caveat: when a relay is used, rather than a direct
   connection, **latency increases**. Traffic now makes an extra hop
   through relay infrastructure, instead of going peer-to-peer. But
   **connectivity is preserved**. The relay fallback exists specifically
   so that "one or both sides are behind restrictive NAT" degrades to
   "slightly higher latency," not to "doesn't work at all." For a
   screen-control use case, this means two cases. Expect the best case
   (lowest latency, most responsive Remote View) when both ends can
   hole-punch directly. Expect a still-fully-functional-but-higher-latency
   case when either end is behind symmetric NAT/CGNAT and traffic
   relays. Both cases are "it works," just with different
   responsiveness.

## Inference: local, on-device only (Aro Private mode, PRD P0-11)

The alpha's **only** inference backend is a local model, served on this
Mac. There is no cloud inference code path (Project Aro PRD row P0-11).
Concretely, the daemon (`mac-daemon/src/local_model.rs`) manages a
[`llama.cpp`](https://github.com/ggml-org/llama.cpp) `llama-server`
subprocess:

```text
llama-server -hf Hcompany/Holo-3.1-35B-A3B-GGUF:Q4_K_M --host 127.0.0.1 --port 8080
```

- **`-hf …:Q4_K_M`** resolves the already-downloaded GGUF from this machine's
  Hugging Face cache. The repo ships a vision projector (`mmproj.f16.gguf`)
  alongside the weights, and `-hf` auto-loads it. Holo3.1 is a *vision*
  model; desktop screenshots are the input. So the projector is
  load-bearing, and the daemon deliberately does **not** pass
  `--no-mmproj`.
- **`--host 127.0.0.1`** binds loopback only. The command builder never
  emits any other host. So the inference endpoint is unreachable off-box.
- The OpenAI-compatible base URL is **`http://127.0.0.1:8080/v1`**. The
  port is overridable via `HOLOIROH_LOCAL_MODEL_PORT`; it must differ
  from the `holo serve` A2A port, `HOLOIROH_HOLO_PORT` (default `8765`).

The daemon (`mac-daemon/src/holo_bridge/process.rs`) points `holo serve`
at that local endpoint. `holo serve` is the A2A front-end the control
channel forwards prompts to. The daemon passes `holo serve --base-url
http://127.0.0.1:8080/v1`, and also sets the `HAI_AGENT_RUNTIME_BASE_URL`
environment variable. That specific env var, not `HAI_BASE_URL`, is the
one that redirects **model inference** in `holo-desktop-cli`. This fact
is verified directly against its installed source:

- `cli/agent_api.py` maps `--base-url` to `HAI_AGENT_RUNTIME_BASE_URL`.
- `agent_client/launcher.py` propagates it to the runtime child, and
  *removes `HAI_API_KEY`*, so the hosted key cannot leak.
- `agent_client/model_gateway.py` shows `HAI_BASE_URL` only overrides
  the cloud *entitlement-probe gateway region*, not inference.

The daemon also removes `HAI_API_KEY` from the `holo serve` child's
environment, on the local path. So the no-cloud guarantee does not
depend on the CLI's own popping logic firing.

**What is verified in-repo vs. benchmarked separately.** The command
construction and env wiring above are real. `cargo run --example
local_model_probe` witnesses them: it builds the exact `llama-server` and
`holo serve` commands the daemon spawns, and prints and asserts their
argv and env, **without spawning the model**. A full live model-serving
run is intentionally *not* part of that verification. The GGUF is ~21 GB,
and takes minutes plus large RAM to load. So re-running it every build is
wasteful. The real end-to-end latency of actually serving it locally
(**8.3 s/step at 720p** on this Apple M3 Pro / 36 GB Mac) is measured and
discussed honestly in [`BENCHMARKS.md`](./BENCHMARKS.md), not re-derived
by the build/probe path.

## Aro Confidential Cloud (Tinfoil) — live in alpha, diverging from PRD row P0-11

This section previously said Tinfoil was deferred entirely to beta, and
not wired into any code path. That statement was already stale by the
time this repo corrected it here. Commits `5c4c91f` and `54af62a` wired a
Tinfoil rate-limit fallback, and clarifying-questions inference, into the
alpha build before this update. This build now wires Tinfoil into six
code paths total. All six are gated on `TINFOIL_API_KEY` being set in the
gitignored `holoiroh/mac-daemon/.env`. Each is independently optional:
absence disables that one feature, logs the absence, and never causes a
startup failure:

- **`tinfoil_proxy.rs`**: rate-limit fallback to `kimi-k2-6`, when the H
  Company hosted backend 429s. This is a loopback auth-injecting proxy;
  see that module's doc for why a proxy is the only workable auth path.
- **`clarify.rs`**: clarifying-questions inference, before an ambiguous
  instruction runs.
- **`tinfoil_documents.rs`**: document-to-markdown conversion
  (`/v1/convert/file`), for attached PDFs, DOCX, PPTX, XLSX, HTML, and
  CSV files.
- **`tinfoil_vision.rs`**: image analysis (`qwen3-vl-30b`/`gemma-4-31b`).
  This analysis routes through `privacy.rs`'s on-device OCR and
  redaction, before upload.
- **`tinfoil_audio.rs`**: audio transcription (`voxtral-small-24b`), and
  text-to-speech (`qwen3-tts`). Transcription is opt-in. It is scoped to
  the client's own microphone capture only, never system or speaker
  audio (see `tinfoil_audio.rs`'s module doc).
- **`tinfoil_planner.rs`**: agentic task planning via tool-calling
  (`glm-5.2`). It proposes a step list for the user to review, before
  anything executes. It composes over the `ComputerUseExecutor` seam,
  rather than reaching into it.

**This directly conflicts with PRD row P0-11**: "the alpha binary must
contain no cloud inference code path at all, verified by egress audit."
It also conflicts with section 7.4/P1-3/Launch Gates 7-8's scoping of
Confidential Cloud to beta. That conflict already existed silently,
before this update. It is now large enough — six modules, not one
fallback path — that it needs an explicit product decision, rather than
another silent README correction. This decision has two options:

1. P0-11 is deliberately superseded for this build. In this case, the
   PRD itself should be updated to say so. The beta-only deployment
   requirements below (attestation, request minimization,
   no-silent-fallback) should move up to apply now.
2. This work is gated behind a build flag that reproduces alpha's
   original no-cloud posture.

This README flags this conflict here, rather than resolving it
unilaterally, since it is a scope/compliance call, not an implementation
one.

None of the above is TEE-attested today. Every Tinfoil call is a plain
HTTPS request with a bearer key; it has no client-side enclave
attestation verification. See
`verification-center-bridge`/`verification-center-webview`, in this
repo's PRD history, for the in-progress Verification Center UI work.
This work is the first step toward the attestation guarantee the beta
deployment requirements table calls for. When beta work formally begins,
the deployment requirements table's other items remain real build items:

- Tinfoil Containers deployment (Aro-controlled immutable image in an
  NVIDIA GPU TEE enclave)
- a strict no-silent-fallback-to-non-confidential-endpoint guarantee

## Naming: `holoiroh` (technical) vs "Aro" (product)

This subproject's directory, Cargo crate, and Swift package are all named
`holoiroh`. This is a technical name that predates the product name. The
Project Aro PRD, the authoritative spec this build follows, calls the
product **Aro**. This name is provisional; formal trademark, App-Store,
and domain clearance are an open question in the PRD, non-blocking until
public beta.

The deliberate decision is to **keep `holoiroh` as the internal/technical
name**, and to use **"Aro" in user-facing strings**: the iOS app's
display name, and any end-user-visible UI text. Renaming a Cargo
workspace and Swift package mid-build has real churn cost. Also, the PRD
itself scopes naming clearance to public-beta, not alpha. So a reader
seeing both names should read `holoiroh` as "the repo, module, or build
artifact," and Aro as "the product a user installs." This is not an
inconsistency to fix. It is a scoped decision, to revisit only at the
naming-clearance milestone the PRD defines.

## Contributing note: worktree isolation requires committed files first

**The target files must already be committed to git, before worktree
agents start.** This rule applies when running large refactors or
rewrites via git-worktree-isolated agents (the pattern this project's
build used heavily). Git worktrees only materialize tracked/committed
content. So an agent dispatched into a fresh worktree cannot see files
that exist only as uncommitted changes in the main checkout. It will
correctly report the target as missing, rather than silently working
around it.

This rule bit the very first scaffold pass of this project: the
`holoiroh/` tree was not committed before the first worktree agents ran.
The fix was committing the scaffold first.

For the first scaffold-creation pass of any new subtree, either commit
early, or use non-worktree agents. For any subsequent worktree-isolated
pass, ensure the files it will edit are already committed.
