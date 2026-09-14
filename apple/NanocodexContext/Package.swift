// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "NanocodexContext",
    platforms: [.iOS(.v17), .macOS(.v14)],
    products: [.library(name: "NanocodexContext", targets: ["NanocodexContext"])],
    dependencies: [.package(path: "../InboxCore")],
    targets: [
        .target(name: "NanocodexContext"),
        .testTarget(name: "NanocodexContextTests", dependencies: ["NanocodexContext", .product(name: "InboxCore", package: "InboxCore")])
    ]
)
