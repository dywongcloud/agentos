# iOS Foreign Function Interface: official Swift bindings for iroh and iroh-live

## Finding

The base `iroh` crate provides official Swift bindings. The `iroh-live` crate does not.

This project uses path (b): the hand-written Rust static library in `holoiroh/ios-bridge/`.
The bridge is a real implementation, not a scaffold. See the as-built sections below.

This document records the research that supports this decision. It also describes these implemented items:

- the subscribe Foreign Function Interface (FFI)
- witnessed builds
- exact XCFramework packaging
- Swift integration

The research used `gh api`, `gh repo view`, and `WebFetch` on 2026-07-17.
It checked the live GitHub repositories, raw README files, podspecs, and manifests.
It also checked the subscribe application binary interface (ABI) against vendored `iroh-live` commit `5f95758`.
It did not use training-data memory. These projects change quickly, so such memory can be stale.

## Items checked

- `n0-computer/iroh` is the base peer-to-peer (P2P) crate.
  The research checked its root, `README`, and `TRANSPORTS.md`.
  It also searched the `n0-computer` GitHub organization for related FFI repositories.
- `n0-computer/iroh-live` provides media streaming over iroh.
  The daemon depends on this crate.
  The research checked its root, `README.md`, `docs/platforms.md`, and each Cargo workspace crate.
- The `n0-computer` organization had 129 repositories.
  The research searched names such as `*-ffi`, `*-swift`, and `*-uniffi`.
  This search found bindings stored outside `iroh` and `iroh-live`.

## Finding (a): base `iroh` has official Swift bindings

The bindings are in a separate repository:
**[`n0-computer/iroh-ffi`](https://github.com/n0-computer/iroh-ffi)**.
Its description is "FFI bindings for iroh."

It wraps `iroh` types such as `Endpoint`, `Connection`, and `EndpointTicket`.
It uses [`uniffi-rs`](https://mozilla.github.io/uniffi-rs/) with `#[derive(uniffi::Object)]` and `#[uniffi::export]` in `src/*.rs`.
One Rust source tree generates Swift, Kotlin, Python, and JavaScript bindings.

The companion repository **[`n0-computer/hello-iroh-ffi`](https://github.com/n0-computer/hello-iroh-ffi)** provides minimal applications.
Its language examples include `swift/`.

The repository contained maintained release infrastructure, not a stub:

- `Package.swift`
- `IrohLib.podspec`
- `IrohLibFramework.podspec`
- `README.swift.md`
- `README.kotlin.md`
- `README.python.md`
- `make_swift.sh`
- `uniffi-bindgen.rs`
- a `.github` release pipeline

The release pipeline builds `IrohLib.xcframework.zip` and attaches it to each GitHub release.

### Swift Package Manager integration

These steps came from `Package.swift` and `README.swift.md`.

`iroh-ffi` uses `swift-tools-version:5.9`.
Its manifest resolves the binary XCFramework in one of two ways:

- For source checkouts and continuous integration (CI), it uses a local XCFramework when present.
- Otherwise, it uses a pinned prebuilt ZIP file from a GitHub release.

The `releaseTag` and `releaseChecksum` constants control the release artifact.
The repository release automation rewrites these constants.

A consumer app adds this Swift Package Manager dependency:

```swift
// In your app's Package.swift, or via Xcode's
// File -> Add Package Dependencies... using the same URL:
dependencies: [
    .package(url: "https://github.com/n0-computer/iroh-ffi.git", from: "1.1.0")
]
```

For a local clone, follow the `README.swift.md` Xcode procedure:

1. Clone `iroh-ffi`.
2. Run `cargo make swift-xcframework`.
   This command requires [`cargo-make`](https://crates.io/crates/cargo-make).
   It builds a release iOS and macOS XCFramework.
   It uses `uniffi-bindgen` and commands such as `cargo build --target aarch64-apple-ios`.
3. In Xcode, add `IrohLib` as a local package dependency.
   The package is the cloned checkout's `ios/` directory.
   This location is effectively the repository root because `Package.swift` is there.
   Add it under **General -> Frameworks, Libraries, and Embedded Content** for the target.
4. Build once.
5. Confirm that `IrohLib` appears in the same list.
   If Xcode removes it, add it again with the `+` button.
   The source identifies this behavior as a known Swift Package Manager quirk with binary targets.
6. Link **`SystemConfiguration`** and **`CoreWLAN`**.
   The `iroh` `netwatch` module calls these frameworks on Apple platforms.
   `Package.swift` also links `Network.framework` for this reason.
   It links `CoreWLAN` only on macOS.
7. Add `import IrohLib` to Swift source.

`Package.swift` specifies these minimum platforms:

- `.iOS("17.5")`
- `.macOS("14.5")`
- `.macCatalyst("17.5")`

### CocoaPods integration

These facts came from `IrohLib.podspec` and `IrohLibFramework.podspec`.

The two pods separate the Swift wrapper from the compiled binary.
The Swift Package Manager manifest uses the same separation.

```ruby
# Podfile
pod 'IrohLib'   # pulls in IrohLibFramework transitively via
                # spec.dependency 'IrohLibFramework', "#{spec.version}"
```

- **`IrohLib`** had version `0.35.0` at research time.
  It contains the Swift wrapper at `IrohLib/Sources/IrohLib/*.swift`.
  It sets `ios.deployment_target '15.0'` and `static_framework = true`.
  It links `SystemConfiguration`.
- **`IrohLibFramework`** had version `0.23.0` at research time.
  Its version changes independently because it tracks the compiled binary release cadence.
  It vendors the prebuilt `Iroh.xcframework`.
  The podspec fetches `".../releases/download/v#{version}/IrohLib.xcframework.zip"` through `spec.source`.

Pin both podspec versions explicitly in the `Podfile`.
Do not assume that their versions match.
The inspected repository used `0.35.0` and `0.23.0`, so the versions were not synchronized.

### Swift API surface

This section comes from the `#[uniffi::export]` items in `src/*.rs`.

`iroh-ffi` exposes transport endpoints, connections, and tickets.
It does not expose a media-stream abstraction because base `iroh` has no such abstraction.
See Finding (b).

UniFFI maps Rust `Result<T, IrohError>` to a throwing Swift function.
It maps `Arc<T>` to a Swift class.
The following example shows the relevant generated surface:

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

A direct fetch of `n0-computer/iroh-ffi/src/` confirmed each source file below.
Each listed item has `#[uniffi::export]`.

- `src/ticket.rs`: `EndpointTicket::from_string`, `::from_addr`, and `.endpoint_addr()`
- `src/endpoint.rs`: `EndpointBuilder::new/apply_n0/bind` and `Endpoint::connect/accept_next/watch_addr/...`
- `src/net.rs`
- `src/key.rs`
- `src/watch.rs`
- `src/relay.rs`
- `src/path.rs`
- `src/accept.rs`

## Finding (b): `iroh-live` has no Swift bindings

The daemon uses **`iroh-live`** for the media layer.
It also uses base `iroh` directly for the control channel.
See `holoiroh/README.md`.

`iroh-live` provides these media APIs:

- `LocalBroadcast`
- `Live::subscribe` and `subscribe_media`
- `Subscription::media()`
- `LiveTicket`

The app needs this surface to receive the Mac screen broadcast.
The research checked these facts directly:

- The `n0-computer/iroh-live` root had no `bindings/`, `ffi/`, `ios/`, `swift/`, or `uniffi/` directory.
  It also had no `Package.swift` or podspec.
- Neither `Cargo.toml` nor `iroh-live/Cargo.toml` had a `uniffi` dependency.
- `docs/platforms.md` was the most likely document for platform support.
  It listed iOS as `"Compiles, untested"`.
  The related stack was `Software and VideoToolbox | AVFoundation | Metal via wgpu`.
  This claim means that the Rust crate compiles for an iOS target.
  The document explicitly described the target as unverified.
  It gave no Swift binding or package-distribution instructions.
  Its next-steps section said, *"iOS: Compiles but untested. Needs on-device validation."*
- The workspace contained one mobile-binding precedent for Android.
  `moq-media-android` is a hand-written Java Native Interface (JNI) bridge crate.
  Its description is "Android camera, EGL rendering, JNI bridge."
  The matching `demos/android/` application uses Kotlin and Rust.
  No `moq-media-ios` counterpart existed.
  This precedent uses a hand-written bridge instead of UniFFI.

Therefore, no official Swift bindings existed for `iroh-live` at research time.
The search found no open issue, roadmap document, or in-progress directory for such bindings.

`iroh-ffi` cannot replace an `iroh-live` binding.
It does not know about `LocalBroadcast`, Media over QUIC (MoQ) subscriptions, or frames.
It stops at raw `Connection`.

If the app used only `iroh-ffi`, it would need a second MoQ implementation in Swift.
That approach would duplicate the Rust behavior in `iroh-live`.
Therefore, `iroh-ffi` is the wrong binding layer for this app.

## Selected path: (b), a Rust static-library bridge

`iroh-live` provides ticket connections, broadcast subscriptions, and frame retrieval.
It has no binding layer.
The hand-written bridge follows the upstream `moq-media-android` pattern.

The bridge is in **`holoiroh/ios-bridge/`** and has these properties:

- It depends directly on `iroh-live` at the daemon's pinned Git commit.
  It also depends on `iroh` for the control channel.
  It parses tickets with `LiveTicket::from_str`.
  It subscribes with `subscribe_with_playback_policy`.
  It obtains video with `subscription.broadcast().video_ready()`.
- It exposes a stable `extern "C"` ABI.
  The ABI supports ticket connection, subscription, frame polling, reachability probes, and control-channel operations from `PROTOCOL.md`.
  Raw pointers carry opaque handles across the FFI boundary.
  An internal Tokio runtime drives asynchronous Rust futures.
  Rust `async` and `await` do not cross the C ABI.
- Device builds use `cargo build --target aarch64-apple-ios`.
  Apple Silicon simulator builds use `aarch64-apple-ios-sim`.
  Intel simulator builds use `x86_64-apple-ios-sim`.
  These commands produce `.a` static libraries.
  `xcodebuild -create-xcframework` combines them into one `.xcframework`.
  This output matches the shape of `iroh-ffi` output.
  This project assembles it manually because `iroh-live` has no UniFFI source.
- cbindgen generates `HoloirohIosBridge.h`.
  After any C ABI change, run `cargo run -p holoiroh-ios-bridge --example generate_header`.
  The include directory also contains `module.modulemap`.
  Swift imports `HoloirohIosBridge` and calls the C functions directly.
  `HoloConnection` and `IrohLiveFrameSource` are the maintained Swift wrappers.

See `holoiroh/ios-bridge/src/lib.rs` for the `extern "C"` implementation.
Its module documentation gives the implementation contract.
The following sections describe the built behavior and witnesses.

## As built: subscribe FFI

The `ios-bridge` crate is not a scaffold.
Its subscribe functions call the real `iroh-live` subscribe API.
All exported C functions have implemented bodies.
The original implementation checked vendored commit `5f95758`.

The exact call chain came from these sources:

- `~/.cargo/git/checkouts/iroh-live-*/5f95758/iroh-live/examples/subscribe_test.rs`
- `frame_dump.rs`
- `iroh-live/src/{live,subscription,ticket}.rs`
- `moq-media/src/subscribe.rs`

### Verified call chain

| Step | Real `iroh-live` API and source |
| --- | --- |
| Bind and create session | `iroh::Endpoint::builder(iroh::endpoint::presets::N0).bind().await` -> `iroh_live::Live::builder(ep).with_router().spawn()`. The `subscribe_test.rs` and `frame_dump.rs` examples use this pattern. |
| Parse ticket | `iroh_live::ticket::LiveTicket::from_str(s)` -> public `endpoint: EndpointAddr` and `broadcast_name: String` fields in `iroh-live/src/ticket.rs`. |
| Connect and subscribe | `live.subscribe(ticket.endpoint, &ticket.broadcast_name).await` -> `iroh_live::Subscription` in `iroh-live/src/live.rs:229`. |
| Get video track | `subscription.broadcast().video_ready().await` -> `moq_media::subscribe::VideoTrack`. It waits for a catalog video rendition. It then selects the best quality and starts decoding. Apple targets use VideoToolbox. See `moq-media/src/subscribe.rs:688`. |
| Pull frame | `track.try_recv()` -> `Option<moq_media::format::VideoFrame>` in `moq-media/src/subscribe.rs:1089`. This nonblocking call drains through the latest frame. |
| Get frame bytes | `frame.rgba_image().as_raw()` -> tightly packed `width*height*4` RGBA8 bytes in `rusty-codecs/src/format.rs:748`. It normalizes packed RGBA, packed BGRA, graphics processing unit (GPU), and NV12 storage. |

The current C surface maps to that chain as follows:

- `holoiroh_ios_bridge_new` creates a generated process-lifetime identity.
- `holoiroh_ios_bridge_new_with_secret_key` creates an identity from exactly 32 seed bytes.
- `holoiroh_ios_bridge_probe_reachable` and `_with_secret_key` probe daemon reachability.
- `holoiroh_ios_bridge_ticket_connect` parses the ticket and subscribes through `iroh-live`.
- `holoiroh_ios_bridge_subscribe` waits through `video_ready`.
- `holoiroh_ios_bridge_poll_next_frame` performs nonblocking `try_recv`.
  It converts RGBA8 to tightly packed BGRA8 during copy-out.
  The caller owns the output buffer.
  `HoloirohFrame` reports `width`, `height`, `timestamp_us`, `pixel_format`, and `kind`.
- `holoiroh_ios_bridge_control_connect`, `_control_send`, and `_poll_control_event` implement the control channel.
- `holoiroh_ios_bridge_subscription_free`, `_free`, and `_free_error_string` release owned resources.

Rust `async` and `await` never cross the C ABI.
The crate owns a Tokio multithread runtime.
Connect and subscribe operations use `block_on`.
Polling uses synchronous `try_recv`.

`catch_unwind` wraps each fallible C entry point.
This prevents a Rust panic from unwinding across the C boundary.
Such unwinding has undefined behavior.
On failure, the function returns a negative `HoloirohStatus`.
When applicable, it also returns a heap error string.
The caller must free that string with `holoiroh_ios_bridge_free_error_string`.

The control channel uses a separate Application-Layer Protocol Negotiation (ALPN) identifier: `holoiroh/control/1`.
The bridge sends the bare personal identification number (PIN) handshake first.
It then signs each client envelope with the persistent endpoint identity.
It verifies each daemon envelope against the authenticated transport peer before Swift can poll it.
See `examples/control_ffi_probe.rs` for the executable fake-daemon witness.

### Witnessed builds

The earlier cross-compilation limitation is superseded.
This environment has Xcode and iPhoneOS software development kit (SDK) 26.4.
The iOS rustup target is installable.
The following executions were witnessed:

- **`cargo build -p holoiroh-ios-bridge` on host `aarch64-apple-darwin`:**
  The command succeeded with **0 warnings**.
  It compiled the complete dependency graph into staticlib and rlib outputs.
  The graph included `iroh-live`, `iroh-moq`, `moq-media`, `rusty-codecs`, `openh264`, and `objc2-av-foundation`.
- **`rustup target add aarch64-apple-ios` followed by `cargo build -p holoiroh-ios-bridge --target aarch64-apple-ios`:**
  The build succeeded with exit status 0.
  It produced `target/aarch64-apple-ios/debug/libholoiroh_ios_bridge.a`.
  At that pass, `nm` listed all nine `_holoiroh_ios_bridge_*` functions as Mach-O text symbols.
  This witness proved that the complete transitive graph cross-compiled for a physical iOS device.
  No crate in the graph blocked the build.
  The current ABI has additional identity, reachability, and implemented control functions.
- **`examples/ffi_probe.rs` with `cargo run --example ffi_probe`:**
  The executable probe replaced a standing unit-test file, per repository rules.
  It exited with status 0 and exercised the C ABI end to end.
  - `_new` returned a non-null handle.
  - A malformed ticket returned `HOLOIROH_ERR_INVALID_TICKET` and an error string.
    The probe freed the string.
  - A valid but unreachable ticket returned `HOLOIROH_ERR_CONNECT_FAILED`.
    Its message was `"No addressing information available"`.
    The real `live.subscribe` dial failed without a panic or hang.
    This sandbox had no reachable iroh relay.
  - `_subscribe` returned null before connection.
  - The C surface accepted null arguments without a crash.
  - At that historical pass, the unimplemented control functions returned `HOLOIROH_ERR_UNSUPPORTED`.
    Current code implements these functions and keeps the constant only for ABI stability.
  - Full teardown completed without a crash or detected leak.
- **`swift build` for the iOS 17 simulator:**
  The command used `--sdk iphonesimulator --triple arm64-apple-ios17.0-simulator`.
  It succeeded.
  `IrohLiveFrameSource.swift` compiled against the real iOS SDK.

### Witness scope

The original headless witness proved these items:

- C ABI error handling and null tolerance
- host and `aarch64-apple-ios` static-library compilation
- exported Mach-O symbols
- construction, error paths, and teardown
- valid C header compilation
- `IrohLiveFrameSource.swift` compilation against the iOS SDK

That witness did not prove an on-screen frame.
It had no reachable live publisher, device run, or final application link.
A real frame requires a reachable daemon media stream through direct or relayed iroh connectivity.

The current repository now contains the installable target at `ios/App/HoloIroh.xcodeproj`.
`ios/Package.swift` links the iOS-only `HoloirohIosBridge` binary target.
Current bridge source records an on-device decoder witness of `20-40fps` during a black-screen diagnosis.
That diagnosis found unsupported `kCVPixelFormatType_32RGBA` pool creation on iOS.
The bridge now swizzles red and blue bytes and emits `HOLOIROH_PIXFMT_BGRA8`.
The source does not record a post-fix, on-screen frame witness.
Therefore, this document does not claim that end-to-end live display is witnessed.

## As built: XCFramework packaging

The app links the static library through an `.xcframework`.
This shape matches the `iroh-ffi` binary target.
This project assembles it manually because `iroh-live` has no UniFFI code generation.

Run these ordered steps from `holoiroh/`:

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

# 4. Regenerate the authoritative C header and module declarations after any
#    Rust C ABI change. The command is repeatable when the header is unchanged.
cargo run -p holoiroh-ios-bridge --example generate_header

# 5. Assemble the xcframework: device slice + fused simulator slice, each
#    paired with the same headers dir (which carries the module map too).
xcodebuild -create-xcframework \
  -library target/aarch64-apple-ios/release/libholoiroh_ios_bridge.a \
  -headers ios-bridge/include \
  -library target/libholoiroh_ios_bridge-sim.a \
  -headers ios-bridge/include \
  -output ios/Artifacts/HoloirohIosBridge.xcframework
```

### Link the XCFramework into the app

`ios/Package.swift` declares the generated XCFramework as the iOS-only `HoloirohIosBridge` binary target.
`ios/App/HoloIroh.xcodeproj` defines the installable app target.
The package provides macOS stubs when the iOS binary module is unavailable.

`HoloConnection` owns one keyed bridge for the control channel and media stream.
It loads one 32-byte iroh seed from the iOS Keychain.
It reuses the seed across launches.
If Keychain access fails, it fails instead of rotating the identity.

The shared `IrohLiveFrameSource` polls decoded BGRA8 frames on a background queue.
It wraps them in pooled `kCVPixelFormatType_32BGRA` buffers for `VideoRenderView`.

The Rust output is a static library, so it does not carry its Apple framework dependencies.
`ios/Package.swift` explicitly links `SystemConfiguration` and `VideoToolbox` for iOS.
Keep those linker settings when integrating the binary into another target.

## Sources consulted

All sources were fetched live instead of recalled from memory.

- `gh repo view n0-computer/iroh`, `gh api repos/n0-computer/iroh/contents`
- `gh repo view n0-computer/iroh-live`
- `gh api repos/n0-computer/iroh-live/contents`
  The request covered the root, `moq-media-android/`, `moq-media/`, `cross/`, and `docs/`.
- `WebFetch` of `raw.githubusercontent.com/n0-computer/iroh-live/main/README.md`
- `WebFetch` and `curl` of `raw.githubusercontent.com/n0-computer/iroh-live/main/docs/platforms.md`
- `gh api orgs/n0-computer/repos --paginate`
  This command returned the complete 129-repository organization listing.
  It found `iroh-ffi`, `hello-iroh-ffi`, `iroh-c-ffi`, and `iroh-js`.
- `gh repo view n0-computer/iroh-ffi`
- `gh api repos/n0-computer/iroh-ffi/contents`
- `gh api repos/n0-computer/hello-iroh-ffi/contents`
- `curl` of `raw.githubusercontent.com/n0-computer/iroh-ffi/main/README.swift.md`
- `curl` of `Package.swift`, `IrohLib.podspec`, and `IrohLibFramework.podspec`
- `gh api repos/n0-computer/iroh-ffi/contents/IrohLib/Sources/IrohLib`
- `gh api repos/n0-computer/iroh-ffi/contents/src`
  This request confirmed `src/{ticket,endpoint,net,key,watch,relay,path,accept}.rs` as the UniFFI-exported surface.
- `curl` of `raw.githubusercontent.com/n0-computer/iroh-ffi/main/src/{ticket,endpoint}.rs`
- `curl` of `raw.githubusercontent.com/n0-computer/iroh-live/main/{Cargo.toml,iroh-live/Cargo.toml,iroh-live/src/{live,subscription,ticket}.rs}`
  These files confirmed that the workspace had no `uniffi` dependency.
  They also confirmed the wrapped `Live::subscribe`, `subscribe_media`, `Subscription::media`, and `LiveTicket` signatures.
