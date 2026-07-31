# iOS FFI: does iroh or iroh-live ship official Swift bindings?

**Finding: the base `iroh` crate does. `iroh-live` (the crate this project
actually depends on) does not. Path taken: (b) -- a hand-written Rust
staticlib crate, `holoiroh/ios-bridge/`. It is now a real implementation,
not a scaffold. See the "As-built" sections below.**

This document records the research behind that decision, so it doesn't
need re-doing. As of the as-built pass, it also documents the real
subscribe FFI, its witnessed builds, and the exact xcframework packaging +
Swift integration.

This research used `gh api`, `gh repo view`, and `WebFetch` on 2026-07-17.
It checked the live GitHub repos and raw READMEs, podspecs, and manifests.
It also verified the as-built subscribe API against the vendored
`iroh-live` source at commit `5f95758`. It did not rely on training-data
memory of these projects. These projects move fast, so memory would likely
be stale.

## What was checked

- `n0-computer/iroh` (base P2P crate) -- repo root contents, README,
  `TRANSPORTS.md`, and a search of the `n0-computer` GitHub org for
  sibling FFI repos.
- `n0-computer/iroh-live` (media-streaming-over-iroh crate this project's
  `mac-daemon` depends on) -- repo root contents, `README.md`,
  `docs/platforms.md`, and every subdirectory/crate in its Cargo workspace.
- `n0-computer` org repo listing (129 repos), searched for names like
  `*-ffi`, `*-swift`, `*-uniffi`, or similar. This search aimed to catch
  bindings that live in a separate repo, not inside `iroh`/`iroh-live`
  themselves. This search found the actual answer.

## Finding (a): official Swift bindings exist for base `iroh`

They live in a **separate repo**, not inside `n0-computer/iroh` itself:
**[`n0-computer/iroh-ffi`](https://github.com/n0-computer/iroh-ffi)**
("FFI bindings for iroh"). It wraps `iroh`'s `Endpoint`/`Connection`/
`EndpointTicket` types with [`uniffi-rs`](https://mozilla.github.io/uniffi-rs/)
(`#[derive(uniffi::Object)]`, `#[uniffi::export]` throughout `src/*.rs`) and
produces Swift, Kotlin, Python, and JS bindings from one Rust source tree.
A companion repo, **[`n0-computer/hello-iroh-ffi`](https://github.com/n0-computer/hello-iroh-ffi)**,
has minimal example apps per language including `swift/`.

This is real, maintained, released infrastructure, not a stub. `iroh-ffi`'s
repo root has:

- `Package.swift`
- `IrohLib.podspec`
- `IrohLibFramework.podspec`
- `README.swift.md`
- `README.kotlin.md`
- `README.python.md`
- `make_swift.sh`
- `uniffi-bindgen.rs`
- a `.github` release pipeline that builds a prebuilt
  `IrohLib.xcframework.zip` and attaches it to each GitHub release

### SwiftPM integration (exact steps, from `Package.swift` + `README.swift.md`)

`iroh-ffi`'s `Package.swift` (`swift-tools-version:5.9`) resolves its
binary xcframework target one of two ways. It uses a locally-built
xcframework if present (source checkouts, CI). Otherwise, it uses a pinned
prebuilt zip attached to a GitHub release. The `releaseTag`/`releaseChecksum`
constants near the top of the file control this, and the repo's own
release automation rewrites them. A consumer app is an app that pulls
`iroh-ffi` in as a dependency. For a consumer app, this resolves to a
plain SwiftPM package dependency:

```swift
// In your app's Package.swift, or via Xcode's
// File -> Add Package Dependencies... using the same URL:
dependencies: [
    .package(url: "https://github.com/n0-computer/iroh-ffi.git", from: "1.1.0")
]
```

or, per `README.swift.md`'s own documented Xcode flow (building from a
local clone rather than the released package):

1. Clone `iroh-ffi`.
2. Run `cargo make swift-xcframework` (requires
   [`cargo-make`](https://crates.io/crates/cargo-make)). This command
   builds a release iOS+macOS xcframework via `uniffi-bindgen` + `cargo
   build --target aarch64-apple-ios` etc. under the hood. See "How
   `iroh-ffi` builds its own xcframework" below for the exact steps this
   triggers.
3. In Xcode, add `IrohLib` (the cloned checkout's `ios/` directory --
   really the repo root, since `Package.swift` lives there) as a **local
   package dependency**. Do this under your target's **General ->
   Frameworks, Libraries, and Embedded Content**.
4. Build once. Confirm `IrohLib` now appears under that same list. Re-add
   it with the `+` button if Xcode dropped it -- a known SwiftPM quirk
   with binary targets.
5. Add **`SystemConfiguration`** and **`CoreWLAN`** as linked frameworks.
   This is required because `iroh`'s `netwatch` module, which detects
   network changes, calls into them on Apple platforms. (`Package.swift`
   also links `Network.framework` for the same reason, plus `CoreWLAN`
   conditionally on macOS only.)
6. Add `import IrohLib` in Swift source.

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
  independently of `IrohLib` since it tracks the release cadence of the
  compiled binary) -- vendors the prebuilt `Iroh.xcframework` fetched via
  `spec.source = { :http => ".../releases/download/v#{version}/IrohLib.xcframework.zip" }`.

Pin both podspec versions explicitly in the `Podfile` when you integrate
them. Do not assume they track each other. The two podspecs' version
numbers are **not** kept in lockstep in the repo, as inspected (0.35.0 vs
0.23.0).

### Swift API surface (from `src/*.rs`'s `#[uniffi::export]` items)

`iroh-ffi` exposes the base transport layer -- endpoints, connections, and
tickets -- not a media-streaming abstraction. That layer does not exist in
`iroh` itself. See Finding (b).

These are the relevant pieces, as they would appear on the Swift side via
uniffi's generated bindings. Rust's `Result<T, IrohError>` becomes a
`throws` Swift function. `Arc<T>` becomes a Swift class:

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

The list below shows the authoritative Rust source for the API above.
Direct fetch of `n0-computer/iroh-ffi`'s `src/` directory confirmed every
file. Every item carries a `#[uniffi::export]` annotation.

- `src/ticket.rs` (`EndpointTicket::from_string`, `::from_addr`,
  `.endpoint_addr()`)
- `src/endpoint.rs` (`EndpointBuilder::new/apply_n0/bind`,
  `Endpoint::connect/accept_next/watch_addr/...`)
- `src/net.rs`
- `src/key.rs`
- `src/watch.rs`
- `src/relay.rs`
- `src/path.rs`
- `src/accept.rs`

## Finding (b): `iroh-live` has no bindings of any kind -- this is the crate that actually matters here

`holoiroh/mac-daemon` depends on **`iroh-live`** for its media layer, not
on base `iroh` directly. It also depends on `iroh` directly, for the
control channel -- see `holoiroh/README.md`. `iroh-live` provides
`LocalBroadcast`, `Live::subscribe`/`subscribe_media`,
`Subscription::media()`, and `LiveTicket`. This is the actual API surface
an iOS client needs to receive the Mac's screen broadcast. Checked
directly:

- `n0-computer/iroh-live`'s repo root: **no** `bindings/`, `ffi/`,
  `ios/`, `swift/`, `uniffi/`, or `Package.swift`/podspec of any kind.
- `Cargo.toml` / `iroh-live/Cargo.toml`: **no** `uniffi` dependency
  anywhere in the workspace.
- `docs/platforms.md` is the doc most likely to mention it, if it existed.
  It lists iOS platform status as `"Compiles, untested"` under `Software
  and VideoToolbox | AVFoundation | Metal via wgpu`. This means the Rust
  crate compiles for an iOS target. The document is explicit that this
  target is unverified. It offers **zero** guidance on language bindings
  or package distribution for Swift. The next-steps section literally
  says *"iOS: Compiles but untested. Needs on-device validation."*
- The one mobile-bindings precedent that *does* exist in this workspace is
  **Android**. It is not uniffi-based either. `moq-media-android` is a
  hand-rolled **JNI bridge crate** ("Android camera, EGL rendering, JNI
  bridge"), with a matching `demos/android/` Kotlin+Rust app. There is no
  `moq-media-ios` counterpart. This is direct evidence of the project's
  own established pattern for mobile bindings when they're needed:
  hand-write the bridge crate, not adopt uniffi. The fallback plan below
  does exactly this for iOS.

Conclusion: **no official Swift bindings exist for `iroh-live`**. Nothing
suggests new bindings are coming: no open issue, roadmap doc, or
in-progress directory references one. `iroh-ffi` cannot substitute for
this. It has no knowledge of `LocalBroadcast`, MoQ subscriptions, or
frames. It stops at raw `Connection`.

Wrapping *only* `iroh-ffi` on the Swift side is not enough by itself. A
consumer must then hand-roll the MoQ/broadcast protocol a second time, in
Swift, on top of raw streams. That reimplements everything `iroh-live`
already solves in Rust. `iroh-ffi` is the wrong layer to bind at.

## Path taken: (b) -- fallback Rust staticlib bridge

`iroh-live` is the crate with the actual functionality this project needs:
ticket-based connect, subscribe to a broadcast, and pull frames. This
crate has no bindings layer. Hand-writing a bridge is this project's own
established pattern (per `moq-media-android` above), not an unusual
choice. Because of this, the fallback plan applies: **`holoiroh/ios-bridge/`**,
a small Rust `staticlib` crate that:

- Depends directly on `iroh-live` (same git-pinned dependency
  `mac-daemon/Cargo.toml` already uses), plus `iroh` for the control
  channel. This lets it call `Live::subscribe`/`Subscription::media()`/
  `LiveTicket::from_str` internally.
- Exposes a small, stable `extern "C"` surface: ticket-connect, subscribe,
  poll-next-frame, plus the control-channel send/recv from `PROTOCOL.md`.
  Opaque handles cross the FFI boundary as raw pointers. A Tokio runtime,
  owned inside the crate, drives the async Rust futures. This runtime is
  not exposed across FFI, because `async`/`await` does not cross a C ABI.
- Builds via `cargo build --target aarch64-apple-ios` (device) and
  `aarch64-apple-ios-sim` / `x86_64-apple-ios-sim` (simulator, Apple
  Silicon and Intel Macs respectively). These builds produce `.a` static
  libraries. `xcodebuild -create-xcframework` combines them into one
  `.xcframework`. This is the same shape `iroh-ffi` itself produces, but
  hand-assembled instead of generated through `uniffi-bindgen`. There is
  no uniffi Rust source to generate from on the `iroh-live` side.
- Ships a hand-written C header, `ios-bridge.h`.
  [`cbindgen`](https://github.com/mozilla/cbindgen) generates this header
  from the `extern "C"` signatures. It also ships a `module.modulemap`, so
  Swift can `import IosBridge` and call the C functions directly. A thin
  hand-written Swift class wraps these calls for ergonomics. This Swift
  wrapper is not committed yet. It is separate follow-on work, to be done
  once the Rust implementations are real, not stubs.

See `holoiroh/ios-bridge/src/lib.rs` for the real `extern "C"`
implementation and its module-level doc comment. The "As-built" section
below records exactly what it does and what was witnessed.

## As-built: the real subscribe FFI

The `ios-bridge` crate is **no longer a scaffold**. Every `extern "C"`
function has a real body, wired to the actual `iroh-live` subscribe API.
This wiring was verified against the vendored crate source at the pinned
commit `5f95758`, not guessed. The exact call chain came from
`~/.cargo/git/checkouts/iroh-live-*/5f95758/iroh-live/examples/subscribe_test.rs`,
`frame_dump.rs`, `iroh-live/src/{live,subscription,ticket}.rs`, and
`moq-media/src/subscribe.rs`.

### The verified call chain

| Step | Real `iroh-live` API (source location) |
| --- | --- |
| Bind + session | `iroh::Endpoint::builder(iroh::endpoint::presets::N0).bind().await` -> `iroh_live::Live::builder(ep).with_router().spawn()` (the exact pattern `subscribe_test.rs`/`frame_dump.rs` use) |
| Parse ticket | `iroh_live::ticket::LiveTicket::from_str(s)` -> a struct with public `endpoint: EndpointAddr` + `broadcast_name: String` (`iroh-live/src/ticket.rs`) |
| Connect + subscribe | `live.subscribe(ticket.endpoint, &ticket.broadcast_name).await` -> `iroh_live::Subscription` (`iroh-live/src/live.rs:229`) |
| Get video track | `subscription.broadcast().video_ready().await` -> `moq_media::subscribe::VideoTrack` (waits for the catalog to advertise a video rendition, then subscribes best-quality and starts the decoder pipeline -- VideoToolbox on Apple targets; `moq-media/src/subscribe.rs:688`) |
| Pull a frame | `track.try_recv()` (non-blocking, drains to the latest) -> `Option<moq_media::format::VideoFrame>` (`moq-media/src/subscribe.rs:1089`) |
| Frame bytes | `frame.rgba_image().as_raw()` -> tightly-packed `width*height*4` RGBA8 `&[u8]`, normalizing any backing pixel format (packed RGBA/BGRA, GPU, NV12) (`rusty-codecs/src/format.rs:748`) |

The C surface maps onto that chain as follows:

- `holoiroh_ios_bridge_new` -- runtime + endpoint bind + `Live` spawn
- `holoiroh_ios_bridge_ticket_connect` -- parse + `live.subscribe`
- `holoiroh_ios_bridge_subscribe` -- `video_ready`
- `holoiroh_ios_bridge_poll_next_frame` -- non-blocking `try_recv`; RGBA8
  bytes into a caller-owned buffer, plus a `HoloirohFrame` metadata struct
  with `width`/`height`/`timestamp_us`/`pixel_format`/`kind`
- explicit `_subscription_free`/`_free`

`async`/`await` never crosses the C ABI. A Tokio multi-thread runtime,
owned inside the crate, drives every async call via `block_on` for
connect/subscribe. Poll uses a synchronous `try_recv` instead.

`catch_unwind` wraps every fallible function, so a Rust panic can never
unwind across the boundary. Unwinding across this boundary is undefined
behavior. Instead, the function returns a negative `HoloirohStatus` plus a
heap error string, freed via `holoiroh_ios_bridge_free_error_string`.

The two control-channel functions, `_control_send`/`_poll_control_event`,
are **not implemented** in this build. The control channel is a separate
iroh ALPN (`holoiroh/control/1`), not part of the media subscribe path.
Because of this, they return `HOLOIROH_ERR_UNSUPPORTED`, never a panic,
until the iOS control transport is built. This work is tracked separately.
See `holoiroh/README.md`'s "Remote kill-switch".

### Witnessed builds (this environment, real execution)

The prior "cross-compilation not available here" note is **superseded**.
This environment has Xcode (iPhoneOS SDK 26.4). The iOS rustup target is
installable here. Witnessed:

- **`cargo build -p holoiroh-ios-bridge` (host `aarch64-apple-darwin`):**
  succeeds, with **0 warnings**. It compiles the full `iroh-live` /
  `iroh-moq` / `moq-media` / `rusty-codecs` / `openh264` /
  `objc2-av-foundation` graph into the staticlib+rlib.
- **`rustup target add aarch64-apple-ios` then `cargo build -p
  holoiroh-ios-bridge --target aarch64-apple-ios`:** **succeeds** (exit 0).
  This produces `target/aarch64-apple-ios/debug/libholoiroh_ios_bridge.a`.
  Running `nm` on it lists all nine `_holoiroh_ios_bridge_*` `extern "C"`
  symbols as Mach-O text symbols. **This is a real finding: the entire
  `iroh-live` transitive dependency graph cross-compiles to a
  physical-device iOS target here.** No crate in the graph blocked it.
- **`examples/ffi_probe.rs`** (`cargo run --example ffi_probe`, no unit
  test file per this repo's rule): exit 0. This probe witnesses the C-ABI
  contract end to end:
  - `_new` returns a non-null handle.
  - A malformed ticket returns `HOLOIROH_ERR_INVALID_TICKET` plus a freed
    error string.
  - A well-formed but unreachable ticket returns
    `HOLOIROH_ERR_CONNECT_FAILED` ("No addressing information available"),
    with no panic or hang. The real `live.subscribe` dial failed cleanly,
    since this sandbox has no reachable iroh relay.
  - Not-connected `_subscribe` returns null.
  - The C surface tolerates null arguments everywhere.
  - The control functions report `HOLOIROH_ERR_UNSUPPORTED`.
  - Full teardown runs with no crash or leak.
- **`swift build` for the iOS 17 simulator** (`--sdk iphonesimulator
  --triple arm64-apple-ios17.0-simulator`): succeeds. `IrohLiveFrameSource.swift`
  compiles against the real iOS SDK.

### What is real vs. still needs a device / network / Xcode-link

**Real and witnessed:**

- the C ABI, its error handling, and its null-tolerance
- the Rust subscribe wiring compiling and linking (host +
  `aarch64-apple-ios` staticlib with the exported symbols)
- the probe exercising construction, error paths, and teardown
- the C header compiling as valid C
- `IrohLiveFrameSource.swift` compiling against the iOS SDK

**Still needed: a real device, a network, and a full Xcode project.**
These are needed for an actual frame to arrive. This requires:

- a live publisher (the Mac daemon), reachable over a real iroh connection
  that is either NAT-punched or relayed
- an Xcode app target that links the `.xcframework` (the one build step
  below)

Headlessly, the dial cannot complete. No `VideoFrame` is produced. So this
is **not** "live video works." Instead: the C ABI, the real subscribe
wiring, and the cross-compile are all real and witnessed. The last mile --
frames on screen -- needs a device, a network, and a link.

## As-built: xcframework packaging (the one build step Xcode needs)

The staticlib becomes an `.xcframework` that the app target links. This is
the same shape that `iroh-ffi`'s own `Iroh` binary target uses. Here, it is
hand-assembled, since there is no uniffi codegen on the `iroh-live` side:

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

`ios/` is a pure SwiftPM package. A pure package cannot produce an
installable `.app` by itself. So a thin Xcode app target, or a SwiftPM
binary target, wraps it.

To wire in the FFI, that target needs exactly one thing: add
**`HoloirohIosBridge.xcframework`** under General -> Frameworks,
Libraries, and Embedded Content. (Alternatively, add a
`.binaryTarget(name: "HoloirohIosBridge", path:
"HoloirohIosBridge.xcframework")` in a `Package.swift`.)

With it linked, `#if canImport(HoloirohIosBridge)` in
`ios/Sources/HoloIrohApp/Video/IrohLiveFrameSource.swift` flips on, and the
real implementation compiles. Without it, the file still builds. The
`#else` branch is a compile-honest stub: it logs "not linked" and produces
no frames. This is why the headless `swift build` above succeeds.

Also link **`SystemConfiguration`** and **`Network.framework`**. This is
required because `iroh`'s `netwatch` module calls into them on Apple
platforms. (These are the same frameworks `iroh-ffi`'s own `Package.swift`
links -- see Finding (a) above.)

Then, at `MainView`'s single binding site, replace
`SyntheticVideoFrameSource()` with `IrohLiveFrameSource(ticket: pastedTicket)`.
`IrohLiveFrameSource` conforms to `VideoFrameSource`. It pulls RGBA8
frames off `holoiroh_ios_bridge_poll_next_frame` on a background queue. It
wraps them in pooled `kCVPixelFormatType_32RGBA` `CVPixelBuffer`s. It
pushes `.pixelBuffer(pb, pts: .invalid)` through the exact same `onFrame`
seam the synthetic source uses. So `VideoRenderView` shows them
display-immediately, with no change to the view.

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
