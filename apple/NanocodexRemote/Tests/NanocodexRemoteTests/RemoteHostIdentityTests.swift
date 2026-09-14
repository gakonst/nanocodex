#if os(macOS)
import XCTest
@testable import NanocodexRemote

final class RemoteHostIdentityTests: XCTestCase {
    func testIdentitySurvivesRecreatingThePreferencesAndPreservesExistingInstallations() throws {
        let suite = "nanocodex.remote.identity-test.\(UUID().uuidString)"
        let first = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { first.removePersistentDomain(forName: suite) }
        let identity = RemoteHostIdentity.load(defaults: first)
        XCTAssertNotNil(UUID(uuidString: identity))
        let relaunched = try XCTUnwrap(UserDefaults(suiteName: suite))
        XCTAssertEqual(RemoteHostIdentity.load(defaults: relaunched), identity)
        first.set("existing-installation", forKey: "nanocodex.remote.machine-id")
        XCTAssertEqual(RemoteHostIdentity.load(defaults: relaunched), "existing-installation")
    }
}
#endif
