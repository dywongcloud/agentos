// swift-tools-version:5.10
import PackageDescription

let package = Package(
    name: "HoloIrohApp",
    platforms: [
        .iOS(.v17)
    ],
    products: [
        .library(
            name: "HoloIrohApp",
            targets: ["HoloIrohApp"]
        )
    ],
    dependencies: [
        // Native Spline runtime for the blue blob orb background scene
        // (SplineOrbBackground.swift). The official splinetool iOS package;
        // 0.2.x is the current release line (latest tag verified via
        // `git ls-remote --tags` before pinning). Native rendering (Metal)
        // rather than the earlier WKWebView web-runtime approach -- the
        // web runtime rendered a degraded orb on device, per live visual
        // comparison against the reference scene render.
        .package(url: "https://github.com/splinetool/spline-ios", from: "0.2.0")
    ],
    targets: [
        // Rust iroh-live subscribe FFI, packaged per IROH_FFI.md's "As-built:
        // xcframework packaging" (device slice built from
        // ../target/aarch64-apple-ios/release/libholoiroh_ios_bridge.a +
        // ../ios-bridge/include). The .xcframework is a BUILD ARTIFACT
        // (gitignored, ~140MB): if it's missing, regenerate with
        //   cargo build --release --target aarch64-apple-ios -p holoiroh-ios-bridge
        //   xcodebuild -create-xcframework \
        //     -library target/aarch64-apple-ios/release/libholoiroh_ios_bridge.a \
        //     -headers ios-bridge/include \
        //     -output ios/Artifacts/HoloirohIosBridge.xcframework
        // (run from holoiroh/).
        .binaryTarget(
            name: "HoloirohIosBridge",
            path: "Artifacts/HoloirohIosBridge.xcframework"
        ),
        .target(
            name: "HoloIrohApp",
            dependencies: [
                // iOS-only: the xcframework carries only an ios-arm64 slice, and
                // IrohLiveFrameSource's `#if canImport(HoloirohIosBridge)` gate
                // falls back to an explanatory stub wherever the module is absent
                // -- so a macOS `swift build` of this package still compiles
                // (stub path), while the Xcode device build links the real
                // bridge and gets live video.
                .target(name: "HoloirohIosBridge", condition: .when(platforms: [.iOS])),
                // iOS-only, same rationale: SplineOrbBackground's
                // `#if canImport(SplineRuntime)` gate keeps the headless
                // macOS `swift build` compiling (gradient-fallback path).
                .product(name: "SplineRuntime", package: "spline-ios", condition: .when(platforms: [.iOS]))
            ],
            path: "Sources/HoloIrohApp",
            exclude: ["REQUIRED_INFO_PLIST_KEYS.md"],
            resources: [
                // The blue-orb Spline scene in the iOS runtime's REAL input
                // format: `.splineswift`, from the editor's Export -> Mobile
                // Platform -> Apple local export. (The previously-bundled
                // `.splinecode` was the WEB runtime's format -- accepted by
                // the URL loader, silently rejected by format validation:
                // the invisible-orb bug.) Bundled so the background renders
                // with zero runtime network dependency. `.copy` (not
                // `.process`): an opaque binary blob the runtime parses.
                .copy("Resources/orb.splineswift")
            ]
        )
    ]
)
