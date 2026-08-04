# Build targets

## Native macOS daemon

Run workspace commands from `holoiroh/`:

```sh
cargo build --workspace
cargo check --workspace --all-targets
```

The daemon is a macOS program. It requires ScreenCaptureKit, VideoToolbox,
Accessibility, and QUIC socket APIs. Grant Screen Recording and Accessibility
in System Settings before a live run.

## iOS application

The Swift package declares iOS 17 and macOS 14. The macOS declaration keeps
host `swift build` checks available; the installable product remains the iOS
`ios/App/HoloIroh.xcodeproj` app. Build an unsigned Simulator artifact with:

```sh
xcodebuild \
  -project ios/App/HoloIroh.xcodeproj \
  -scheme HoloIroh \
  -sdk iphonesimulator \
  -destination 'generic/platform=iOS Simulator' \
  ARCHS=arm64 ONLY_ACTIVE_ARCH=YES CODE_SIGNING_ALLOWED=NO \
  build
```

See `ios/IROH_FFI.md` for Rust static-library and xcframework commands.

## WASI wire-protocol artifact

`holoiroh-wire` and `holoiroh-wire-wasm-demo` support `wasm32-wasip1`. The full
daemon and iOS bridge do not support WASI. Their networking graphs use
`socket2`, which intentionally emits a compile error for WASI Preview 1, and
the daemon also depends on macOS-only capture and media frameworks.

Install a clang toolchain with a WebAssembly backend and the WASI C sysroot:

```sh
brew install llvm wasi-libc
rustup target add wasm32-wasip1
```

Apple's Xcode clang does not include a WebAssembly backend. Set the C compiler,
archiver, and sysroot for C-backed transitive crates:

```sh
export CC_wasm32_wasip1="$(brew --prefix llvm)/bin/clang"
export AR_wasm32_wasip1="$(brew --prefix llvm)/bin/llvm-ar"
export CFLAGS_wasm32_wasip1="--sysroot=$(brew --prefix wasi-libc)/share/wasi-sysroot"
```

Build and run the real protocol artifact:

```sh
cargo build -p holoiroh-wire-wasm-demo --target wasm32-wasip1
wasmtime target/wasm32-wasip1/debug/holoiroh-wire-wasm-demo.wasm
```

The demo serializes and round-trips a `TaskEnvelope`, accepts its first inbound
sequence, and rejects replayed, non-increasing, and expired envelopes. Keep WASI
work scoped to platform-neutral protocol crates instead of adding ineffective
configuration gates around the daemon's required socket and Apple APIs.
