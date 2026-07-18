# iOS FFI: does iroh or iroh-live ship official Swift bindings?

**Finding: the base `iroh` crate does; `iroh-live` (the crate this project
actually depends on) does not. Path taken: (b), the fallback plan --
a hand-written Rust staticlib crate, `holoiroh/ios-bridge/`.**

This document records the research behind that decision so it doesn't need
re-doing, and describes the fallback's shape. Researched 2026-07-17 via
`gh api`/`gh repo view` against the live GitHub repos and `WebFetch` against
raw READMEs/podspecs/manifests -- not from training-data memory of these
projects, which move fast enough that memory would likely be stale.

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

See `holoiroh/ios-bridge/src/lib.rs` for the actual scaffolded
`extern "C"` signatures (currently `unimplemented!()` bodies -- wiring
them to real `iroh-live` calls is separate follow-on work, per the task
that requested this scaffold) and its module-level doc comment for the
exact xcframework packaging command sequence.

## Environment note: cross-compilation not available here

`rustup target list --installed` in this environment shows only
`aarch64-apple-darwin`, `aarch64-unknown-linux-gnu`, and three `wasm32`/
`x86_64-unknown-linux-*` targets -- **no** `aarch64-apple-ios` or
`aarch64-apple-ios-sim`, and `cargo-lipo` is not installed. This means the
`ios-bridge` crate scaffolded here has been verified with `cargo check -p
ios-bridge` on the host triple only; actually cross-compiling to an iOS
target (`cargo build --target aarch64-apple-ios`) and producing the
`.xcframework` requires a macOS machine with Xcode installed and the
iOS/iOS-simulator rustup targets added (`rustup target add
aarch64-apple-ios aarch64-apple-ios-sim`) -- a separate, later step, not
something this environment can complete or fake evidence of.

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
