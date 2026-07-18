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
        .target(
            name: "HoloIrohApp",
            path: "Sources/HoloIrohApp"
        )
    ]
)
