// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "NanocodexUI",
    platforms: [.macOS(.v14), .iOS(.v17)],
    products: [.library(name: "NanocodexUI", targets: ["NanocodexUI"])],
    dependencies: [
        .package(url: "https://github.com/appstefan/HighlightSwift.git", from: "1.1.0"),
    ],
    targets: [
        .target(name: "NanocodexUI", dependencies: ["HighlightSwift"]),
        .testTarget(name: "NanocodexUITests", dependencies: ["NanocodexUI"]),
    ]
)
