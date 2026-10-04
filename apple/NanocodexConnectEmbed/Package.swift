// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "NanocodexConnectEmbed",
    platforms: [.iOS(.v17), .macOS(.v14)],
    products: [.library(name: "NanocodexConnectEmbed", targets: ["NanocodexConnectEmbed"])],
    dependencies: [
        .package(path: "../NanocodexUI"),
        .package(path: "../NanocodexRemote"),
        .package(url: "https://github.com/ekazaev/ChatLayout.git", exact: "2.5.2"),
    ],
    targets: [
        .target(name: "NanocodexConnectEmbed", dependencies: [
            "NanocodexUI", "NanocodexRemote",
            .product(name: "ChatLayout", package: "ChatLayout", condition: .when(platforms: [.iOS])),
        ]),
    ],
    swiftLanguageModes: [.v5]
)
