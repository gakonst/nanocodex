// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "NanocodexChat",
    platforms: [.macOS(.v14), .iOS(.v17)],
    products: [.library(name: "NanocodexChat", targets: ["NanocodexChat"])],
    dependencies: [.package(path: "../InboxCore")],
    targets: [
        .target(name: "NanocodexChat", dependencies: ["InboxCore"]),
        .testTarget(name: "NanocodexChatTests", dependencies: ["NanocodexChat", "InboxCore"])
    ]
)
