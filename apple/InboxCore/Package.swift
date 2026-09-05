// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "InboxCore",
    platforms: [.iOS(.v17), .macOS(.v14)],
    products: [.library(name: "InboxCore", targets: ["InboxCore"])],
    targets: [.target(name: "InboxCore"), .testTarget(name: "InboxCoreTests", dependencies: ["InboxCore"])]
)
