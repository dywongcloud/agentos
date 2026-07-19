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
                .target(name: "HoloirohIosBridge", condition: .when(platforms: [.iOS]))
            ],
            path: "Sources/HoloIrohApp",
            exclude: ["REQUIRED_INFO_PLIST_KEYS.md"]
        )
    ]
)
