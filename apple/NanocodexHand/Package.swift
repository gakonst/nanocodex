// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "NanocodexHand",
    platforms: [.iOS(.v17), .macOS(.v14)],
    products: [.library(name: "NanocodexHand", targets: ["NanocodexHand"])],
    dependencies: [.package(path: "../InboxCore"), .package(path: "../NanocodexContext")],
    targets: [
        .target(name: "NanocodexHand", dependencies: ["InboxCore", "NanocodexContext"]),
        .testTarget(name: "NanocodexHandTests", dependencies: ["NanocodexHand", "InboxCore", "NanocodexContext"])
    ]
)
