// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "NanocodexChatUI",
    platforms: [.macOS("15.0"), .iOS("18.0")],
    products: [.library(name: "NanocodexChatUI", targets: ["NanocodexChatUI"])],
    dependencies: [
        .package(path: "../NanocodexChat"),
        .package(path: "../NanocodexUI"),
        .package(path: "../InboxCore"),
    ],
    targets: [.target(name: "NanocodexChatUI", dependencies: ["NanocodexChat", "NanocodexUI", "InboxCore"])]
)
