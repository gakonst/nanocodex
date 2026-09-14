// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "NanocodexVoice",
    platforms: [.iOS(.v17), .macOS(.v14)],
    products: [.library(name: "NanocodexVoice", targets: ["NanocodexVoice"])],
    dependencies: [
        .package(path: "../InboxCore"),
        .package(url: "https://github.com/stasel/WebRTC.git", exact: "152.0.0"),
    ],
    targets: [
        .binaryTarget(name: "NanocodexVoiceCore", path: "Artifacts/NanocodexVoiceCore.xcframework"),
        .target(name: "NanocodexVoice", dependencies: ["InboxCore", "NanocodexVoiceCore", .product(name: "WebRTC", package: "WebRTC")]),
        .testTarget(name: "NanocodexVoiceTests", dependencies: ["NanocodexVoice", "InboxCore"]),
    ],
    swiftLanguageModes: [.v5]
)
