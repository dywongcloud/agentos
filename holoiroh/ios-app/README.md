# HoloIroh iOS app wrapper (device deploy)

This wrapper is a thin Xcode APP target around the `../ios` Swift package,
plus the Rust iroh-live bridge xcframework. The Swift package holds the real
`@main` and all app code. This wrapper exists because SPM cannot produce an
iOS `.app` bundle. It is what gets signed and installed on a phone. The app
was first deployed to a physical iPhone 14 (iOS 26.5.2) on 2026-07-19. It was
personal-team signed. It was witnessed running.

## One-time prerequisites (already done on this Mac)

- Apple ID signed into Xcode (Settings -> Accounts) -> personal team
  `XBBQ2LGY3K`, pinned in `project.yml` (`DEVELOPMENT_TEAM`).
- On the iPhone, turn on Developer Mode.
- After the first install, trust the developer cert via Settings -> General
  -> VPN & Device Management.
- `brew install xcodegen` (generates `HoloIroh.xcodeproj` from `project.yml`.
  The xcodeproj is gitignored. Regenerate it. Never hand-edit it.)

## Full rebuild + redeploy (the whole pipeline)

```sh
cd holoiroh

# 1. Rust bridge staticlib (device slice)
cargo build --release --target aarch64-apple-ios -p holoiroh-ios-bridge

# 2. Repackage the xcframework the Swift package's binaryTarget points at
#    (gitignored build artifact; required for `import HoloirohIosBridge`
#    to resolve so the app gets the LIVE video path, not the stub)
rm -rf ios/Artifacts/HoloirohIosBridge.xcframework
xcodebuild -create-xcframework \
  -library target/aarch64-apple-ios/release/libholoiroh_ios_bridge.a \
  -headers ios-bridge/include \
  -output ios/Artifacts/HoloirohIosBridge.xcframework

# 3. Regenerate the Xcode project (only needed after editing project.yml)
cd ios-app && xcodegen generate

# 4. Signed build for the plugged-in iPhone (device id via
#    `xcrun devicectl list devices`)
xcodebuild -project HoloIroh.xcodeproj -scheme HoloIroh \
  -destination 'id=<DEVICE-UDID>' -allowProvisioningUpdates build

# 5. Install + launch
APP=~/Library/Developer/Xcode/DerivedData/HoloIroh-*/Build/Products/Debug-iphoneos/HoloIroh.app
xcrun devicectl device install app --device <DEVICE-UDID> $APP
xcrun devicectl device process launch --device <DEVICE-UDID> com.dylanwong.holoiroh
```

## Gotchas (each one hit for real during the first deploy)

- **Personal-team apps expire after 7 days.** Rerun steps 4-5 to re-sign.
- **`canImport(HoloirohIosBridge)` is decided at PACKAGE compile time.** So
  the bridge must be a package `binaryTarget` (see `../ios/Package.swift`). It
  cannot be an app-target link flag. Without the xcframework present, the
  package still builds everywhere via the stub path (platform-conditioned
  dep).
- **Opaque FFI handles must be INCOMPLETE C types.** A defined struct imports
  into Swift as `UnsafeMutablePointer<T>`. This is true even for a zero-sized
  struct (`uint8_t _private[0]`). This breaks the wrapper's `OpaquePointer`
  fields. Forward declarations import as `OpaquePointer` instead. This was
  fixed in `../ios-bridge/include/HoloirohIosBridge.h` the first time the
  build genuinely imported the module.
- The staticlib links against the `SystemConfiguration`, `Security`, and
  `Network` frameworks. It also links against `libc++` (openh264) and
  `libresolv`. These are declared as sdk dependencies in `project.yml`.
  Missing dependencies surface as undefined-symbol link errors.
- Rust objects carry the SDK's own min-iOS (26.4). This causes ld warnings on
  a 17.0-deployment app. The warnings are harmless as long as the device runs
  >= that iOS. Pass `IPHONEOS_DEPLOYMENT_TARGET` to the cargo build to
  silence them properly.
