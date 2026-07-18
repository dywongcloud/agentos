# iOS FFI: does iroh or iroh-live ship official Swift bindings?

**Finding: the base `iroh` crate does; `iroh-live` (the crate this project
actually depends on) does not. Path taken: (b) -- a hand-written Rust
staticlib crate, `holoiroh/ios-bridge/`, which is now a real
implementation (not a scaffold): see the "As-built" sections below.**

This document records the research behind that decision so it doesn't need
re-doing, and (as of the as-built pass) documents the real subscribe FFI,
its witnessed builds, and the exact xcframework packaging + Swift
integration. The research was done 2026-07-17 via `gh api`/`gh repo view`
against the live GitHub repos and `WebFetch` against raw
READMEs/podspecs/manifests; the as-built subscribe API was verified against
the vendored `iroh-live` source at commit `5f95758` -- neither from
training-data memory of these projects, which move fast enough that memory
would likely be stale.

## What was checked

- `n0-computer/iroh` (base P2P crate) -- repo root contents, README,
  `TRANSPORTS.md`, and a search of the `n0-computer` GitHub org for
  sibling FFI repos.
- `n0-computer/iroh-live` (media-streaming-over-iroh crate this project's
  `mac-daemon` depends on) -- repo root contents, `README.md`,
  `docs/platforms.md`, and every subdirectory/crate in its Cargo workspace.
- `n0-computer` org repo listing (129 repos) for anything named
  `*-ffi`, `*-swift`, `*-uniffi`, or similar, to catch bindings that live
  in a separate repo rather than inside `iroh`/`iroh-live` themselves --
  this is where the actual answer was found.

## Finding (a): official Swift bindings exist for base `iroh`

They live in a **separate repo**, not inside `n0-computer/iroh` itself:
**[`n0-computer/iroh-ffi`](https://github.com/n0-computer/iroh-ffi)**
("FFI bindings for iroh"). It wraps `iroh`'s `Endpoint`/`Connection`/
`EndpointTicket` types with [`uniffi-rs`](https://mozilla.github.io/uniffi-rs/)
(`#[derive(uniffi::Object)]`, `#[uniffi::export]` throughout `src/*.rs`) and
produces Swift, Kotlin, Python, and JS bindings from one Rust source tree.
A companion repo, **[`n0-computer/hello-iroh-ffi`](https://github.com/n0-computer/hello-iroh-ffi)**,
has minimal example apps per language including `swift/`.

This is real, maintained, released infrastructure -- not a stub:
`iroh-ffi`'s repo root has `Package.swift`, `IrohLib.podspec`,
`IrohLibFramework.podspec`, `README.swift.md`, `README.kotlin.md`,
`README.python.md`, `make_swift.sh`, `uniffi-bindgen.rs`, and a
`.github` release pipeline that builds and attaches a prebuilt
`IrohLib.xcframework.zip` to each GitHub release.

### SwiftPM integration (exact steps, from `Package.swift` + `README.swift.md`)

`iroh-ffi`'s `Package.swift` (`swift-tools-version:5.9`) resolves its
binary xcframework target one of two ways: a locally-built xcframework if
present (source checkouts, CI), otherwise a pinned prebuilt zip attached to
a GitHub release (`releaseTag`/`releaseChecksum` constants near the top of
the file, rewritten by the repo's own release automation). For a consumer
app -- i.e. what an app pulling this in as a dependency actually does --
that resolves to a plain SwiftPM package dependency:

```swift
// In your app's Package.swift, or via Xcode's
// File -> Add Package Dependencies... using the same URL:
dependencies: [
    .package(url: "https://github.com/n0-computer/iroh-ffi.git", from: "1.1.0")
]
```

or, per `README.swift.md`'s own documented Xcode flow (building from a
local clone rather than the released package):

1. Clone `iroh-ffi`, run `cargo make swift-xcframework` (requires
   [`cargo-make`](https://crates.io/crates/cargo-make); builds a release
   iOS+macOS xcframework via `uniffi-bindgen` + `cargo build --target
   aarch64-apple-ios` etc. under the hood -- see "How `iroh-ffi` builds its
   own xcframework" below for the exact steps this triggers).
2. In Xcode, add `IrohLib` (the cloned checkout's `ios/` directory --
   really the repo root, since `Package.swift` lives there) as a **local
   package dependency** under your target's **General -> Frameworks,
   Libraries, and Embedded Content**.
3. Build once; confirm `IrohLib` now appears under that same list (re-add
   with the `+` button if Xcode dropped it, a known SwiftPM quirk with
   binary targets).
4. Add **`SystemConfiguration`** and **`CoreWLAN`** as linked frameworks --
   required because `iroh`'s `netwatch` network-change-detection module
   calls into them on Apple platforms (`Package.swift` also links
   `Network.framework` for the same reason, plus `CoreWLAN` conditionally
   on macOS only).
5. `import IrohLib` in Swift source.

Platform floor per `Package.swift`: `.iOS("17.5")`, `.macOS("14.5")`,
`.macCatalyst("17.5")`.

### CocoaPods integration (from `IrohLib.podspec` + `IrohLibFramework.podspec`)

Two pods, split the same way the SwiftPM manifest splits Swift wrapper
code from the compiled binary:

```ruby
# Podfile
pod 'IrohLib'   # pulls in IrohLibFramework transitively via
                # spec.dependency 'IrohLibFramework', "#{spec.version}"
```

- **`IrohLib`** (version `0.35.0` at research time) -- the Swift source
  wrapper (`IrohLib/Sources/IrohLib/*.swift`), `ios.deployment_target
  '15.0'`, `static_framework = true`, links `SystemConfiguration`.
- **`IrohLibFramework`** (version `0.23.0` at research time, versioned
  independently of `IrohLib` since it tracks the compiled binary release
  cadence) -- vendors the prebuilt `Iroh.xcframework` fetched via
  `spec.source = { :http => ".../releases/download/v#{version}/IrohLib.xcframework.zip" }`.

Note the two podspecs' version numbers are **not** kept in lockstep in the
repo as inspected (0.35.0 vs 0.23.0) -- when integrating, pin both
explicitly in the `Podfile` rather than assuming they track each other.

### Swift API surface (from `src/*.rs`'s `#[uniffi::export]` items)

`iroh-ffi` exposes the base transport layer -- endpoints, connections,
tickets -- not a media-streaming abstraction (that layer doesn't exist in
`iroh` itself; see Finding (b)). The relevant pieces, as they'd appear on
the Swift side via uniffi's generated bindings (Rust `Result<T, IrohError>`
becomes a `throws` Swift function; `Arc<T>` becomes a Swift class):

```swift
// Ticket: connect
let ticket = try EndpointTicket(fromString: pastedOrScannedString)
let addr = ticket.endpointAddr()

// Endpoint: bind + connect (roughly; exact generated names depend on the
// uniffi Swift binding's casing convention -- see endpoint.rs for the
// authoritative Rust signatures this generates from)
let builder = EndpointBuilder()
builder.applyN0()                       // n0's default relay/discovery config
let endpoint = try await builder.bind()
let connection = try await endpoint.connect(addr: addr, alpn: alpnBytes)

// No "subscribe" or "next_frame" equivalent exists here -- iroh-ffi wraps
// raw QUIC (Endpoint/Connection/send-recv streams), not a pub/sub media
// broadcast. A consumer would have to build a subscribe/frame protocol
// on top of Connection's bidi/uni streams itself, which is exactly what
// iroh-live does in Rust -- and exactly what has no Swift equivalent.
```

Authoritative Rust source for the above (all `#[uniffi::export]`-annotated,
confirmed via direct fetch of `n0-computer/iroh-ffi` `src/`):
`src/ticket.rs` (`EndpointTicket::from_string`, `::from_addr`,
`.endpoint_addr()`), `src/endpoint.rs` (`EndpointBuilder::new/apply_n0/bind`,
`Endpoint::connect/accept_next/watch_addr/...`), `src/net.rs`, `src/key.rs`,
`src/watch.rs`, `src/relay.rs`, `src/path.rs`, `src/accept.rs`.

## Finding (b): `iroh-live` has no bindings of any kind -- this is the crate that actually matters here

`holoiroh/mac-daemon` depends on **`iroh-live`**, not on base `iroh`
directly for its media layer (it does also depend on `iroh` directly, for
the control channel -- see `holoiroh/README.md`). `iroh-live` is what
provides `LocalBroadcast`, `Live::subscribe`/`subscribe_media`,
`Subscription::media()`, and `LiveTicket` -- the actual API surface an iOS
client needs to receive the Mac's screen broadcast. Checked directly:

- `n0-computer/iroh-live`'s repo root: **no** `bindings/`, `ffi/`,
  `ios/`, `swift/`, `uniffi/`, or `Package.swift`/podspec of any kind.
- `Cargo.toml` / `iroh-live/Cargo.toml`: **no** `uniffi` dependency
  anywhere in the workspace.
- `docs/platforms.md` (the doc most likely to mention it if it existed):
  lists iOS platform status as `"Compiles, untested"` under
  `Software and VideoToolbox | AVFoundation | Metal via wgpu` -- i.e. the
  Rust crate itself is known to at least compile with an iOS target, but
  the document is explicit that this is unverified and offers **zero**
  guidance on language bindings or package distribution for Swift. Next-
  steps section literally says *"iOS: Compiles but untested. Needs
  on-device validation."*
- The one mobile-bindings precedent that *does* exist in this workspace is
  **Android**, and it is not uniffi-based either: `moq-media-android` is a
  hand-rolled **JNI bridge crate** ("Android camera, EGL rendering, JNI
  bridge"), with a matching `demos/android/` Kotlin+Rust app. There is no
  `moq-media-ios` counterpart. This is direct evidence of the project's own
  established pattern for mobile bindings when they're needed: hand-write
  the bridge crate, not adopt uniffi -- which is exactly what the fallback
  plan below does for iOS.

Conclusion: **no official Swift bindings exist for `iroh-live`**, and none
are implied to be forthcoming (no open issue, roadmap doc, or in-progress
directory found referencing one). `iroh-ffi` cannot substitute for this --
it doesn't know about `LocalBroadcast`, MoQ subscriptions, or frames at
all; it stops at raw `Connection`. Wrapping *only* `iroh-ffi` on the Swift
side and then trying to hand-roll the MoQ/broadcast protocol a second time
in Swift on top of raw streams would mean reimplementing everything
`iroh-live` already solves in Rust -- the wrong layer to bind at.

## Path taken: (b) -- fallback Rust staticlib bridge

Because `iroh-live` (the crate with the actual functionality this project
needs -- ticket-based connect, subscribe to a broadcast, pull frames) has
no bindings layer, and hand-writing a bridge is this project's own
established pattern (per `moq-media-android` above) rather than an
unusual choice, the fallback plan applies: **`holoiroh/ios-bridge/`**, a
small Rust `staticlib` crate that:

- Depends directly on `iroh-live` (same git-pinned dependency
  `mac-daemon/Cargo.toml` already uses) plus `iroh` for the control
  channel, so it can call `Live::subscribe`/`Subscription::media()`/
  `LiveTicket::from_str` internally.
- Exposes a small, stable `extern "C"` surface -- ticket-connect,
  subscribe, poll-next-frame, plus the control-channel send/recv from
  `PROTOCOL.md` -- opaque handles crossing the FFI boundary as raw
  pointers, async Rust futures driven by a Tokio runtime owned inside the
  crate (not exposed across FFI; `async`/`await` doesn't cross a C ABI).
- Builds via `cargo build --target aarch64-apple-ios` (device) and
  `aarch64-apple-ios-sim` / `x86_64-apple-ios-sim` (simulator, Apple
  Silicon and Intel Macs respectively) into `.a` static libraries, which
  `xcodebuild -create-xcframework` combines into one `.xcframework` --
  the same shape `iroh-ffi` itself produces, but hand-assembled instead of
  going through `uniffi-bindgen`, since there's no uniffi Rust source to
  generate from on the `iroh-live` side.
- Ships a hand-written C header (`ios-bridge.h`, via
  [`cbindgen`](https://github.com/mozilla/cbindgen) generating it from the
  `extern "C"` signatures) and a `module.modulemap` so Swift can `import
  IosBridge` and call the C functions directly, wrapped in a thin
  hand-written Swift class for ergonomics (not committed yet -- Swift-side
  wrapper is separate follow-on work once the Rust implementations are
  real, not stubs).

See `holoiroh/ios-bridge/src/lib.rs` for the real `extern "C"`
implementation (the "As-built" section below records exactly what it does
and what was witnessed) and its module-level doc comment.

## As-built: the real subscribe FFI

The `ios-bridge` crate is **no longer a scaffold** -- every `extern "C"`
function has a real body wired to the actual `iroh-live` subscribe API,
verified against the vendored crate source at the pinned commit `5f95758`
(not guessed -- the exact call chain was read out of
`~/.cargo/git/checkouts/iroh-live-*/5f95758/iroh-live/examples/subscribe_test.rs`,
`frame_dump.rs`, `iroh-live/src/{live,subscription,ticket}.rs`, and
`moq-media/src/subscribe.rs`).

### The verified call chain

| Step | Real `iroh-live` API (source location) |
| --- | --- |
| Bind + session | `iroh::Endpoint::builder(iroh::endpoint::presets::N0).bind().await` -> `iroh_live::Live::builder(ep).with_router().spawn()` (the exact pattern `subscribe_test.rs`/`frame_dump.rs` use) |
| Parse ticket | `iroh_live::ticket::LiveTicket::from_str(s)` -> a struct with public `endpoint: EndpointAddr` + `broadcast_name: String` (`iroh-live/src/ticket.rs`) |
| Connect + subscribe | `live.subscribe(ticket.endpoint, &ticket.broadcast_name).await` -> `iroh_live::Subscription` (`iroh-live/src/live.rs:229`) |
| Get video track | `subscription.broadcast().video_ready().await` -> `moq_media::subscribe::VideoTrack` (waits for the catalog to advertise a video rendition, then subscribes best-quality and starts the decoder pipeline -- VideoToolbox on Apple targets; `moq-media/src/subscribe.rs:688`) |
| Pull a frame | `track.try_recv()` (non-blocking, drains to the latest) -> `Option<moq_media::format::VideoFrame>` (`moq-media/src/subscribe.rs:1089`) |
| Frame bytes | `frame.rgba_image().as_raw()` -> tightly-packed `width*height*4` RGBA8 `&[u8]`, normalizing any backing pixel format (packed RGBA/BGRA, GPU, NV12) (`rusty-codecs/src/format.rs:748`) |

The C surface maps that onto: `holoiroh_ios_bridge_new` (runtime + endpoint
bind + `Live` spawn) -> `holoiroh_ios_bridge_ticket_connect` (parse +
`live.subscribe`) -> `holoiroh_ios_bridge_subscribe` (`video_ready`) ->
`holoiroh_ios_bridge_poll_next_frame` (non-blocking `try_recv`, RGBA8 bytes
into a caller-owned buffer + a `HoloirohFrame` metadata struct with
`width`/`height`/`timestamp_us`/`pixel_format`/`kind`) -> explicit
`_subscription_free`/`_free`. `async`/`await` never crosses the C ABI: a
Tokio multi-thread runtime owned inside the crate drives every async call
via `block_on` for connect/subscribe, while poll is a synchronous
`try_recv`. Every fallible function is wrapped in `catch_unwind` so a Rust
panic can never unwind across the boundary (undefined behavior); it returns
a negative `HoloirohStatus` + a heap error string (freed via
`holoiroh_ios_bridge_free_error_string`) instead.

The two control-channel functions (`_control_send`/`_poll_control_event`)
are honestly **not implemented** in this build -- the control channel is a
separate iroh ALPN (`holoiroh/control/1`), not part of the media subscribe
path -- so they return `HOLOIROH_ERR_UNSUPPORTED` (never a panic) until the
iOS control transport is built (tracked separately; see
`holoiroh/README.md`'s "Remote kill-switch").

### Witnessed builds (this environment, real execution)

The prior "cross-compilation not available here" note is **superseded** --
this environment has Xcode (iPhoneOS SDK 26.4) and the iOS rustup target is
installable. Witnessed:

- **`cargo build -p holoiroh-ios-bridge` (host `aarch64-apple-darwin`):**
  succeeds, **0 warnings** -- compiles the full `iroh-live` / `iroh-moq` /
  `moq-media` / `rusty-codecs` / `openh264` / `objc2-av-foundation` graph
  into the staticlib+rlib.
- **`rustup target add aarch64-apple-ios` then `cargo build -p
  holoiroh-ios-bridge --target aarch64-apple-ios`:** **succeeds** (exit 0).
  Produces `target/aarch64-apple-ios/debug/libholoiroh_ios_bridge.a`, and
  `nm` on it lists all nine `_holoiroh_ios_bridge_*` `extern "C"` symbols as
  Mach-O text symbols. **This is a real finding: the entire `iroh-live`
  transitive dependency graph cross-compiles to a physical-device iOS
  target here** -- no crate in the graph blocked it.
- **`examples/ffi_probe.rs` (`cargo run --example ffi_probe`, no unit test
  file per this repo's rule):** exit 0. Witnesses the C-ABI contract end to
  end -- `_new` returns a non-null handle; a malformed ticket returns
  `HOLOIROH_ERR_INVALID_TICKET` + a freed error string; a well-formed but
  unreachable ticket returns `HOLOIROH_ERR_CONNECT_FAILED` ("No addressing
  information available" -- the real `live.subscribe` dial failed cleanly,
  since this sandbox has no reachable iroh relay) with no panic/hang;
  not-connected `_subscribe` returns null; null args are tolerated across
  the surface; the control fns report `HOLOIROH_ERR_UNSUPPORTED`; and full
  teardown runs with no crash/leak.
- **`swift build` for the iOS 17 simulator** (`--sdk iphonesimulator
  --triple arm64-apple-ios17.0-simulator`): succeeds -- `IrohLiveFrameSource.swift`
  compiles against the real iOS SDK.

### What is real vs. still needs a device / network / Xcode-link

**Real and witnessed:** the C ABI, its error handling and null-tolerance,
the Rust subscribe wiring compiling and linking (host + `aarch64-apple-ios`
staticlib with the exported symbols), the probe exercising construction /
error paths / teardown, the C header compiling as valid C, and
`IrohLiveFrameSource.swift` compiling against the iOS SDK.

**Still needs a real device + network + a full Xcode project:** an actual
frame arriving. That requires a live publisher (the Mac daemon) reachable
over a real NAT-punched/relayed iroh connection, plus an Xcode app target
that links the `.xcframework` (the one build step below). Headlessly, the
dial cannot complete and no `VideoFrame` is produced -- so this is **not**
"live video works." It is: the C ABI, the real subscribe wiring, and the
cross-compile are all real and witnessed; the last mile (frames on screen)
needs a device + network + link.

## As-built: xcframework packaging (the one build step Xcode needs)

The staticlib becomes an `.xcframework` the app target links. This is the
same shape `iroh-ffi`'s own `Iroh` binary target uses, hand-assembled since
there is no uniffi codegen on the `iroh-live` side:

```sh
cd holoiroh
# 1. iOS rustup targets (device + both simulator arches). aarch64-apple-ios
#    was installed and its build witnessed this session; the two sim targets
#    are the same shape.
rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios-sim

# 2. A release staticlib per target.
cargo build -p holoiroh-ios-bridge --release --target aarch64-apple-ios
cargo build -p holoiroh-ios-bridge --release --target aarch64-apple-ios-sim
cargo build -p holoiroh-ios-bridge --release --target x86_64-apple-ios-sim

# 3. Fuse the two simulator slices into one fat binary (an xcframework slice
#    must be a single binary, but "simulator" covers arm64 + Intel Macs).
lipo -create \
  target/aarch64-apple-ios-sim/release/libholoiroh_ios_bridge.a \
  target/x86_64-apple-ios-sim/release/libholoiroh_ios_bridge.a \
  -output target/libholoiroh_ios_bridge-sim.a

# 4. The C header + module map already live in ios-bridge/include/ (committed:
#    HoloirohIosBridge.h + module.modulemap). Regenerate the header's type
#    section any time the extern "C" signatures change:
#      (cd ios-bridge && cbindgen --config cbindgen.toml \
#         --crate holoiroh-ios-bridge --output include/HoloirohIosBridge.h)
#    then re-append the hand-kept function-prototype block (cbindgen 0.27 skips
#    edition-2024 `#[unsafe(no_mangle)]` fns -- see the header's own note).

# 5. Assemble the xcframework: device slice + fused simulator slice, each
#    paired with the same headers dir (which carries the module map too).
xcodebuild -create-xcframework \
  -library target/aarch64-apple-ios/release/libholoiroh_ios_bridge.a \
  -headers ios-bridge/include \
  -library target/libholoiroh_ios_bridge-sim.a \
  -headers ios-bridge/include \
  -output HoloirohIosBridge.xcframework
```

### Linking it into the app (the single remaining Xcode step)

`ios/` is a pure SwiftPM package; a pure package can't itself produce an
installable `.app`, so a thin Xcode app target (or a SwiftPM binary target)
wraps it. To wire the FFI in, that target needs exactly one thing: **add
`HoloirohIosBridge.xcframework` under General -> Frameworks, Libraries, and
Embedded Content** (or a `.binaryTarget(name: "HoloirohIosBridge", path:
"HoloirohIosBridge.xcframework")` in a `Package.swift`). With it linked,
`#if canImport(HoloirohIosBridge)` in
`ios/Sources/HoloIrohApp/Video/IrohLiveFrameSource.swift` flips on and the
real implementation compiles; without it, the file still builds (the
`#else` branch is a compile-honest stub that logs "not linked" and produces
no frames), which is why the headless `swift build` above succeeds. Because
`iroh`'s `netwatch` calls into them on Apple platforms, also link
**`SystemConfiguration`** and **`Network.framework`** (the same frameworks
`iroh-ffi`'s own `Package.swift` links -- see Finding (a) above).

Then swap the frame source at `MainView`'s single binding site: replace
`SyntheticVideoFrameSource()` with `IrohLiveFrameSource(ticket: pastedTicket)`.
`IrohLiveFrameSource` conforms to `VideoFrameSource`, pulls RGBA8 frames off
`holoiroh_ios_bridge_poll_next_frame` on a background queue, wraps them in
pooled `kCVPixelFormatType_32RGBA` `CVPixelBuffer`s, and pushes
`.pixelBuffer(pb, pts: .invalid)` through the exact same `onFrame` seam the
synthetic source uses -- so `VideoRenderView` shows them display-immediately
with no change to the view.

## Sources consulted (all fetched live, not from memory)

- `gh repo view n0-computer/iroh`, `gh api repos/n0-computer/iroh/contents`
- `gh repo view n0-computer/iroh-live`,
  `gh api repos/n0-computer/iroh-live/contents` (root + `moq-media-android/`,
  `moq-media/`, `cross/`, `docs/`)
- `WebFetch` of `raw.githubusercontent.com/n0-computer/iroh-live/main/README.md`
- `WebFetch` + `curl` of `raw.githubusercontent.com/n0-computer/iroh-live/main/docs/platforms.md`
- `gh api orgs/n0-computer/repos --paginate` (full 129-repo org listing --
  found `iroh-ffi`, `hello-iroh-ffi`, `iroh-c-ffi`, `iroh-js` this way)
- `gh repo view n0-computer/iroh-ffi`,
  `gh api repos/n0-computer/iroh-ffi/contents`,
  `gh api repos/n0-computer/hello-iroh-ffi/contents`
- `curl` of `raw.githubusercontent.com/n0-computer/iroh-ffi/main/README.swift.md`,
  `Package.swift`, `IrohLib.podspec`, `IrohLibFramework.podspec`
- `gh api repos/n0-computer/iroh-ffi/contents/IrohLib/Sources/IrohLib`,
  `.../contents/src` (confirms `src/{ticket,endpoint,net,key,watch,relay,
  path,accept}.rs` as the uniffi-exported surface)
- `curl` of `raw.githubusercontent.com/n0-computer/iroh-ffi/main/src/{ticket,endpoint}.rs`
- `curl` of `raw.githubusercontent.com/n0-computer/iroh-live/main/{Cargo.toml,
  iroh-live/Cargo.toml,iroh-live/src/{live,subscription,ticket}.rs}` --
  confirms no `uniffi` dependency anywhere, and the exact
  `Live::subscribe`/`subscribe_media`/`Subscription::media`/`LiveTicket`
  signatures the fallback bridge wraps.
