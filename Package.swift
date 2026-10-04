// swift-tools-version: 6.0
import PackageDescription

// Remote SwiftPM entry point. Targets compile the same source files used by the
// local Apple packages and first-party apps; no vendored renderer or transport.
let package = Package(
    name: "NanocodexConnectSDK",
    platforms: [.iOS(.v17), .macOS(.v14)],
    products: [
        .library(name: "NanocodexConnectEmbed", targets: ["NanocodexConnectEmbed"]),
    ],
    dependencies: [
        .package(url: "https://github.com/PhoneNumberKit/PhoneNumberKit", from: "5.0.8"),
        .package(url: "https://github.com/groue/GRDB.swift.git", exact: "7.8.0"),
        .package(url: "https://github.com/appstefan/HighlightSwift.git", from: "1.1.0"),
        .package(url: "https://github.com/kean/Nuke.git", exact: "13.2.0"),
        .package(url: "https://github.com/gonzalezreal/swift-markdown-ui.git", exact: "2.4.1"),
        .package(url: "https://github.com/stasel/WebRTC.git", exact: "152.0.0"),
        .package(url: "https://github.com/ekazaev/ChatLayout.git", exact: "2.5.2"),
    ],
    targets: [
        .target(name: "InboxCore", dependencies: [
            .product(name: "PhoneNumberKit", package: "PhoneNumberKit"),
            .product(name: "GRDB", package: "GRDB.swift"),
        ], path: "apple/InboxCore/Sources/InboxCore"),
        .target(name: "NanocodexUI", dependencies: [
            "HighlightSwift", .product(name: "Nuke", package: "Nuke"),
            .product(name: "MarkdownUI", package: "swift-markdown-ui", condition: .when(platforms: [.iOS])),
        ], path: "apple/NanocodexUI/Sources/NanocodexUI"),
        .target(name: "NanocodexRemote", dependencies: [
            "InboxCore", .product(name: "WebRTC", package: "WebRTC"),
        ], path: "apple/NanocodexRemote/Sources/NanocodexRemote"),
        .target(name: "NanocodexConnectEmbed", dependencies: [
            "NanocodexUI", "NanocodexRemote",
            .product(name: "ChatLayout", package: "ChatLayout", condition: .when(platforms: [.iOS])),
        ], path: "apple/NanocodexConnectEmbed/Sources/NanocodexConnectEmbed"),
    ],
    swiftLanguageModes: [.v5]
)
