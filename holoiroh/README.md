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
that same doc but not yet implemented. Startup now also does two real,
wired preflight checks before any capture/publish work begins: a Holo
auth-token check (`auth.rs`, refuses to start with a clear instruction if
`holo login` was never run) and a macOS Screen Recording + Accessibility
TCC permission preflight (`permissions.rs`, same refuse-with-instructions
behavior rather than starting into a black/frozen stream). `holo_bridge/`
also now runs an ongoing health-check loop (`holo_bridge/health.rs`) that
polls the supervised `holo serve` subprocess and restarts it on crash,
independent of the one-time startup health check `process.rs` already did.
System/mic audio capture is still not wired up. `ios/` is now a real
multi-screen SwiftUI app skeleton that builds for iOS 17: `ContentView`
hosts a `NavigationStack` moving from
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

The control channel's wire schema now wraps every message (except the bare
pre-session PIN handshake) in a `TaskEnvelope` -- `protocol_version`,
`message_id`, `session_id`, `task_id`, `sent_at`/`expires_at`,
`sequence_number`, `payload`, `signature` -- matching the Project Aro PRD's
authoritative envelope shape. This is **real, wired code**, not a paper
schema: `control_channel.rs` actually rejects inbound envelopes that are
expired, replay a seen `message_id`, or send a non-increasing
`sequence_number`, in that order, before the payload is even parsed (see
[`PROTOCOL.md`](./PROTOCOL.md)'s "Envelope" and "Rejection rules"
sections). `signature` rides on the wire per the PRD schema but is **not
cryptographically verified** -- there is no signing-keypair infrastructure
in this codebase yet, so the field is always `null` on envelopes this
daemon constructs and unchecked on the way in. A new message pair,
`input_request` (server → client) / `input_response` (client → server),
lets the daemon pause a running turn to ask the user a structured question
(credential needed, MFA needed, ambiguous choice, missing info, or
sensitive-access consent) -- also real and wired, including a real timed
expiry-to-safe-pause path (an unanswered request emits a `status`, never an
`error`, and clears itself rather than hanging). Credentials/MFA codes
themselves never travel on this channel by construction -- see
[`PROTOCOL.md`](./PROTOCOL.md)'s "Credentials never travel on this
channel" for why, and why the real secret-entry path (a separate
`manual_input` channel) remains unbuilt.

Two new modules add PRD-tracked functionality that is real, working, and
independently witnessed, but **not yet wired to a live policy or event
source** -- both say so explicitly in their own doc comments and this
README repeats that honestly rather than rounding up to "implemented":

- **`sensitive_categories.rs`** (PRD §9, class-5 "sensitive target" apps
  like password managers, banking, health, system settings) is a real data
  model and config-file (`~/.holoiroh/sensitive_categories.toml` or
  `.json`) for a per-category always-ask/always-allow/hard-block setting,
  with real bundle-ID classification. Its own module doc is explicit: *"This
  is a config-file row, not a policy-enforcement row... nothing in this
  codebase currently calls into this module from a live interception
  point."* There is no `ComputerUseExecutor`/policy-wrapper equivalent in
  this Rust daemon yet -- `holo_bridge` still forwards every prompt straight
  through to `holo serve` with no pause-before-sensitive-surface check.
- **`limits.rs`** (PRD §10.4 session/rate limits) -- see the dedicated
  "Session & rate limits" section below for the exact per-limit
  real-vs-constant-only breakdown; the short version is the same pattern:
  most limits are typed, independently-tested constants/helpers with no
  live call site wiring them into an actual session/turn yet, and two
  (max active tasks per Mac, agent action cap) **are** really enforced
  today.

Two more new modules are real and independently witnessed with a different
shape of gap each:

- **`audit_log.rs`** (PRD row P0-12, local metadata-only audit log) writes
  a real, append-only JSON-Lines file (default `~/.holoiroh/audit.log`) via
  a real `AuditLogger` type. `AuditEntry` has exactly the ten PRD-named
  fields and deliberately no catch-all string/JSON field, so it is
  structurally impossible for a call site to log a dictated transcript,
  prompt text, or recipient name -- `examples/audit_log_probe.rs`'s
  acceptance test proves this by writing a real audit entry then grepping
  the literal on-disk bytes for a marker string and confirming it's absent.
  **Not yet called from `main.rs`/`control_channel.rs`/`holo_bridge`** --
  nothing in the live request path constructs an `AuditEntry` today, so no
  audit log is actually produced by running the daemon; two of its ten
  fields (`app_category`, `remote_view_state`) are also honestly modeled as
  single-variant enums for now, since this daemon has no per-app attribution
  and no way to detach the broadcast independently of the control channel
  yet (see the module's own "Real vs. honestly-approximated fields" doc).
  The `inference_mode` field's `Local` variant is now the accurate value for
  this build's actual inference path (see `local_model.rs` below and the
  "Inference: local, on-device only" section) -- but since `audit_log` has
  no live call site at all yet, no `AuditEntry` is constructed anywhere to
  carry it, so nothing sets it either; the wiring gap is the missing call
  site, not the enum.
- **`task_state.rs`** (PRD task lifecycle: 16 flow states + 4 interactive-
  waiting states + 10 terminal states) is a real, fully-modeled Rust enum
  with a real, exhaustively-tested `is_valid_transition` state machine
  (including the three Confidential-Cloud/Tinfoil states, which are
  present for schema completeness but provably unreachable in this
  alpha build). It is **deliberately independent of any live event
  source** -- `holo_bridge::control::ControlEvent`/`DoneStatus` (the only
  task-progress types actually wired to the real `holo serve` A2A stream
  today) report three coarse outcomes plus free-text progress strings, with
  no concept of which of `task_state.rs`'s finer states a task is in.
  Promoting live events to carry a real `TaskState` needs `holo-desktop-cli`
  itself to expose that granularity, which it does not today.

`mac-daemon` has **no `#[cfg(test)]` unit tests** as of this writing --
they were deliberately removed (this repo's no-unit-tests rule: validation
must be real, witnessed execution, not assertions run later) and
`cargo test -p holoiroh-daemon` now runs **0 tests**. Their coverage was
re-witnessed as `cargo run --example <name>_probe` binaries instead --
`allowlist_probe`, `auth_probe`, `auth_gate_probe`, `control_channel_probe`,
`envelope_probe`, `input_request_probe`, `task_state_probe`,
`audit_log_probe`, `sensitive_categories_probe`, `limits_probe`,
`holo_bridge_queue_probe`, `permissions_probe`, and `local_model_probe`
(builds the exact `llama-server` + `holo serve` subprocess commands and
verifies the local-inference env wiring **without spawning the 21 GB
model**), alongside the pre-existing `control_probe` (a real external `iroh`
dial against a live daemon) -- see "Build status" below for exact, witnessed
build and probe results. The daemon's actual inference path is the
**on-device `llama-server` local model** (see "Inference: local, on-device
only" below); a real end-to-end latency benchmark of that path (8.3 s/step
@ 720p on this Mac) was run separately -- because a live model-serving run
loads ~21 GB and takes minutes, it is documented in
[`BENCHMARKS.md`](./BENCHMARKS.md) rather than re-run by the build/probe
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
│       └── local_model_probe.rs           # builds the exact llama-server + holo serve commands and verifies the local-inference env wiring, WITHOUT spawning the 21 GB model
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

**`cargo build --workspace` in `holoiroh/mac-daemon`: succeeds, warning-clean.**

```
Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.92s
```

(Re-witnessed this session after `sensitive_categories.rs`, `audit_log.rs`,
`task_state.rs`, and the `TaskEnvelope`/`input_request` additions to
`control_channel.rs` — `grep -c warning` on a forced rebuild's output is
`0`.)

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
succeeds, including the `[lib]` target added so `examples/control_probe.rs`
can dial the control channel as an external `iroh` peer. There are no
`#[cfg(test)]` unit tests in this crate as of this writing** — they were
deliberately removed (`cargo test -p holoiroh-daemon` now runs 0 tests;
see "Status" above) and their coverage re-witnessed as `cargo run
--example <name>_probe` binaries instead:

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

This probe exercises real `serde_json::to_string`/`from_str` round-trips
for every `ClientMessage`/`ServerMessage` variant against the exact JSON
[`PROTOCOL.md`](./PROTOCOL.md) specifies (optional `text` present/omitted,
malformed/unknown-`type` input producing a `serde_json::Error` rather than
a panic), plus the `ControlEvent` → `ServerMessage` translation.
`examples/allowlist_probe.rs` (real temp-file round-trips for
`Allowlist::load`/`save`/`add_entry`/`remove_entry`, PIN
generation/verification, and the security-relevant "corrupt file fails
closed, not open" case) and `examples/permissions_probe.rs`
(`PreflightResult`/`MissingPermission` construction and instruction text,
including real `stderr` output) were run the same way, with all cases
passing.

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

**Startup auth/permission preflight (`auth.rs`, `permissions.rs`) and the
`holo serve` health-check loop (`holo_bridge/health.rs`): real, wired into
`main.rs`/`HoloBridge`, and unit-level-witnessed via `examples/auth_probe.rs`,
`examples/auth_gate_probe.rs`, and `examples/permissions_probe.rs`** (see
above) — token-file parsing, PIN/allowlist gate logic, and
`PreflightResult`/`MissingPermission` construction were each exercised
against real strings/files/in-memory state with passing output. A live,
end-to-end run of `holoiroh-daemon` itself on macOS hardware with Screen
Recording/Accessibility actually granted (to confirm the preflight passes
cleanly and the daemon proceeds to publish) was **not** re-witnessed in
this pass; treat that specific end-to-end path as real-but-not-freshly-
verified until it is.

**`TaskEnvelope`, `input_request`/`input_response`, `sensitive_categories.rs`,
`audit_log.rs`, `task_state.rs`, and `limits.rs`: each independently
witnessed via its own probe, all passing.**

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

`examples/audit_log_probe.rs` is real and its underlying module logic
passes cleanly (writes real JSON-Lines entries, proves append-only
behavior, and — the PRD P0-12 acceptance test — greps the literal on-disk
bytes for a dictated-text marker string and confirms it's absent). **One
honest wrinkle found while re-witnessing this pass**: the probe's own
first assertion (`subdir must not exist yet for this to be a real test`)
panicked on a stale run, because its "parent directory gets created"
case uses a **fixed, non-unique** subdirectory name
(`holoiroh-audit-probe-subdir`) left over in `$TMPDIR` from an earlier run
in the same session — a real bug in the probe's own temp-path hygiene
(every other path in this probe suite mixes in a PID + nanosecond
timestamp; this one line doesn't), not a defect in `audit_log.rs` itself.
Deleting the stale directory and re-running produced a full clean pass:

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

The fixed-dirname reuse bug in the probe itself has since been fixed
(the subdir name now mixes in a PID + nanosecond timestamp, matching the
rest of the probe suite's temp-path scheme), so the probe is idempotent --
verified by running it twice back-to-back without clearing `$TMPDIR`, both
passing cleanly.

`cargo run --example holo_bridge_queue_probe` was also re-run this pass and
still passes for the same reason documented previously: real
concurrent-prompt-queueing logic is witnessed against an unreachable A2A
endpoint, while the full live-daemon-plus-live-`holo-serve` path remains
blocked in this sandbox by the same two pre-existing causes (no
Accessibility TCC grant, no `holo` CLI on `PATH`) — not a regression from
this pass's changes. `cargo test -p holoiroh-daemon` still runs 0 tests.

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

## Session & rate limits (PRD 10.4)

`mac-daemon/src/limits.rs` is the single source of truth for every numeric
limit Project Aro PRD section 10.4 specifies, each a named constant with a
doc comment citing 10.4 directly. As with the rest of this README, the
honest real-vs-designed breakdown matters more than a bare "implemented"
claim, so here is the exact status of each:

| PRD 10.4 limit | Value | Status |
| --- | --- | --- |
| Task request expiry | 30s default | Constant only. `ClientMessage` (`PROTOCOL.md`) carries no timestamp field to compute request age against; needs a wire-schema change (tracked under `holoiroh-task-envelope-protocol`). |
| Active session lifetime | 10min max | Constant + real `SessionTimer` type (independently exercised, see `examples/limits_probe.rs`), not called from a live call site -- there is no persistent "session" object spanning multiple control-channel connections yet. |
| Approval token | 60s TTL, one task + one action | Constant + real `ApprovalToken` type (single-use, TTL-checked, task-scoped; independently exercised), not wired to a live call site -- there is no approval-gating flow in this codebase yet. |
| Heartbeat | every 5s while active | Constant only. `ClientMessage`/`ServerMessage` have no heartbeat message variant; the natural insertion point is documented as a doc comment on `ControlChannel::accept`'s `tokio::select!`. |
| Disconnect handling | pause after 5s, cancel after 15s unless safely draft-complete | Constants only. Connection loss is already detected in `ControlChannel::accept`'s read loop, but it tears the connection down immediately today -- no pause-then-cancel grace period, and no "safely draft-complete" task-state concept exists yet (see `holoiroh-task-state-machine-terminal-statuses`). |
| Max active tasks per Mac | 1 | **Really enforced.** This was already the exact behavior of `HoloControlBridge`'s pre-existing `busy`/`queue` mechanism (a second prompt while one is in flight is queued, never run concurrently) -- `limits.rs` now names that behavior explicitly and a `debug_assert!` in `handle_prompt` ties the constant to the `bool`-shaped enforcement it models. |
| Max active controllers per Mac | 1 | **Gap found, honestly reported, not silently fixed.** `ControlChannel::accept` does not reject a second simultaneous connection from an already-allowlisted device -- it runs the same accept path independently and both connections can coexist, with only the most recent sender's connection receiving `ControlEvent`s (via the existing `replace_event_sink` reconnect-redirect mechanism, which was designed for "old connection dropped, new one takes over," not "two connections alive at once"). Not wired here because a real fix changes accept-time rejection behavior for an already-allowlisted device and needs a product decision on which connection should win; see `limits.rs`'s `MAX_ACTIVE_CONTROLLERS_PER_MAC` doc for the exact code path and the proposed fix shape. |
| Task runtime | 45s default / 120s max | Constants + a real `clamp_task_runtime` function (independently exercised: an over-max request is actually clamped, not passed through), not wired into `HoloControlBridge::run_prompt` -- that function has no per-task deadline/timeout concept today (`send_and_stream` runs to completion with no `tokio::time::timeout` wrapper). |
| Agent action cap | 100 default | **Really enforced.** `ActionCounter` (real, atomic, independently exercised -- refuses a 101st `try_record`) is constructed per turn in `HoloControlBridge::run_prompt` and counts every `TaskUpdate::Working` update; once the cap is hit, further progress events for that turn are suppressed and the turn ends with a `ControlEvent::Error` reporting the cap. **Documented limitation:** this does not stop `holo serve` from continuing to run the agent server-side past the 100th action -- `A2aClient::send_and_stream`'s callback has no way to signal "abort the stream," and a real `tasks/cancel` needs the resolved `context_id`, which is only available after the stream ends. A true server-side abort needs a callback-contract change out of this pass's scope. |
| Manual input rate | 120 events/s max | Constant only, no channel to attach it to yet. This codebase's wire schema has no `manual_input` message type at all (only `Prompt`/`VoiceTranscript`/`Stop`/`Pin`); the richer 6-stream protocol PRD 7.1 describes (which includes a dedicated `manual_input` stream) is tracked separately under `holoiroh-task-envelope-protocol`. |

Verification: `cargo run --example limits_probe` exercises `ActionCounter`,
`SessionTimer`, `ApprovalToken`, and `clamp_task_runtime` directly with real
execution (not a test file, per this repo's convention) -- all four passed
in the witnessed run this section is based on. `cargo build` in
`mac-daemon` stays warning-clean with all of the above in place.

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

## Inference: local, on-device only (Aro Private mode, PRD P0-11)

The alpha's **only** inference backend is a local model served on this Mac —
there is no cloud inference code path (Project Aro PRD row P0-11). Concretely,
the daemon (`mac-daemon/src/local_model.rs`) manages a
[`llama.cpp`](https://github.com/ggml-org/llama.cpp) `llama-server`
subprocess:

```text
llama-server -hf Hcompany/Holo-3.1-35B-A3B-GGUF:Q4_K_M --host 127.0.0.1 --port 8080
```

- **`-hf …:Q4_K_M`** resolves the already-downloaded GGUF from this machine's
  Hugging Face cache. The repo ships a vision projector (`mmproj.f16.gguf`)
  alongside the weights, and `-hf` auto-loads it (Holo3.1 is a *vision* model
  — desktop screenshots are the input — so the projector is load-bearing and
  the daemon deliberately does **not** pass `--no-mmproj`).
- **`--host 127.0.0.1`** binds loopback only; the command builder never emits
  any other host, so the inference endpoint is unreachable off-box.
- The OpenAI-compatible base URL is **`http://127.0.0.1:8080/v1`**
  (port overridable via `HOLOIROH_LOCAL_MODEL_PORT`; it must differ from the
  `holo serve` A2A port, `HOLOIROH_HOLO_PORT`, default `8765`).

`holo serve` (the A2A front-end the control channel forwards prompts to) is
pointed at that local endpoint by the daemon
(`mac-daemon/src/holo_bridge/process.rs`), which passes
`holo serve --base-url http://127.0.0.1:8080/v1` **and** sets the
`HAI_AGENT_RUNTIME_BASE_URL` environment variable. That specific env var — not
`HAI_BASE_URL` — is the one that redirects **model inference** in
`holo-desktop-cli`: verified directly against its installed source
(`cli/agent_api.py` maps `--base-url`→`HAI_AGENT_RUNTIME_BASE_URL`;
`agent_client/launcher.py` propagates it to the runtime child *and removes
`HAI_API_KEY`* so the hosted key can't leak; `agent_client/model_gateway.py`
shows `HAI_BASE_URL` only overrides the cloud *entitlement-probe gateway
region*, not inference). The daemon also removes `HAI_API_KEY` from the
`holo serve` child's environment on the local path, so the no-cloud guarantee
does not depend on the CLI's own popping logic firing.

**What is verified in-repo vs. benchmarked separately.** The command
construction and env wiring above are real and are witnessed by
`cargo run --example local_model_probe`, which builds the exact `llama-server`
and `holo serve` commands the daemon would spawn and prints/asserts their
argv and env — **without spawning the model**. A full live model-serving run
is intentionally *not* part of that verification: the GGUF is ~21 GB and takes
minutes plus large RAM to load, so re-running it every build would be
wasteful. The real end-to-end latency of actually serving it locally
(**8.3 s/step at 720p** on this Apple M3 Pro / 36 GB Mac) is measured and
discussed honestly in [`BENCHMARKS.md`](./BENCHMARKS.md), not re-derived by
the build/probe path.

## Deferred to beta: Aro Confidential Cloud (Tinfoil)

The Project Aro PRD (section 7.4, P1-3, Launch Gates 7-8) scopes the
Tinfoil-based Confidential Cloud inference mode to **beta (Phase 2)**, not
alpha. Per PRD row P0-11, the alpha binary must contain **no cloud inference
code path at all** (verified by egress audit), and Aro Private mode (local
Holo3.1 via llama.cpp) is the only alpha inference mode. A Tinfoil API key
was supplied during development but, by explicit decision, is **not wired
into any code path** — it lives only in the gitignored
`holoiroh/mac-daemon/.env` and no `.rs`/`.swift`/`.toml`/`.md` tracked source
references `TINFOIL_API_KEY` (confirmed by grep). When beta work begins, this
becomes a real build item: Tinfoil Containers deployment (Aro-controlled
immutable image in an NVIDIA GPU TEE enclave), client-side attestation before
any content leaves the Mac, request minimization, and a strict no-silent-
fallback-to-non-confidential-endpoint guarantee, per the PRD's deployment
requirements table.

## Naming: `holoiroh` (technical) vs "Aro" (product)

This subproject's directory, Cargo crate, and Swift package are all named
`holoiroh` — a technical name that predates the product name. The Project
Aro PRD (the authoritative spec this build follows) calls the product
**Aro** (provisional, formal trademark/App-Store/domain clearance is an
open question in the PRD, non-blocking until public beta). The deliberate
decision is to **keep `holoiroh` as the internal/technical name** (renaming
a Cargo workspace + Swift package mid-build has real churn cost and the PRD
itself scopes naming clearance to public-beta, not alpha) while using
**"Aro" in user-facing strings** — the iOS app's display name, any
end-user-visible UI text. So a reader seeing both names should read
`holoiroh` = "the repo/module/build artifact" and Aro = "the product a user
installs." This is not an inconsistency to fix; it's a scoped decision to
revisit only at the naming-clearance milestone the PRD defines.

## Contributing note: worktree isolation requires committed files first

When running large refactors or rewrites via git-worktree-isolated agents
(the pattern this project's build used heavily), the target files **must
already be committed to git** before the worktree agents start. Git
worktrees only materialize tracked/committed content, so an agent dispatched
into a fresh worktree cannot see files that exist only as uncommitted
changes in the main checkout — it will correctly report the target as
missing rather than silently working around it. This bit the very first
scaffold pass of this project (the `holoiroh/` tree wasn't committed before
the first worktree agents ran); the fix was committing the scaffold first.
For the first scaffold-creation pass of any new subtree, either commit
early or use non-worktree agents; for any subsequent worktree-isolated pass,
ensure the files it will edit are already committed.
