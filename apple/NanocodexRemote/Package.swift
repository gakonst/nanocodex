// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "NanocodexRemote",
    platforms: [.iOS(.v17), .macOS(.v14)],
    products: [.library(name: "NanocodexRemote", targets: ["NanocodexRemote"])],
    dependencies: [
        .package(url: "https://github.com/stasel/WebRTC.git", exact: "152.0.0"),
    ],
    targets: [
        .target(name: "NanocodexRemote", dependencies: [.product(name: "WebRTC", package: "WebRTC")]),
        .testTarget(name: "NanocodexRemoteTests", dependencies: ["NanocodexRemote"]),
    ],
    swiftLanguageModes: [.v5]
)
