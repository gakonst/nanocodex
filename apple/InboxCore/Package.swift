// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "InboxCore",
    platforms: [.iOS(.v17), .macOS(.v14)],
    products: [.library(name: "InboxCore", targets: ["InboxCore"])],
    dependencies: [.package(url: "https://github.com/PhoneNumberKit/PhoneNumberKit", from: "5.0.8")],
    targets: [
        .target(name: "InboxCore", dependencies: [.product(name: "PhoneNumberKit", package: "PhoneNumberKit")]),
        .testTarget(name: "InboxCoreTests", dependencies: ["InboxCore"], resources: [.copy("Fixtures")])
    ]
)
