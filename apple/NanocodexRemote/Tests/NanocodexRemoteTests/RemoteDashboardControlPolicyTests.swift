import XCTest
@testable import NanocodexRemote

final class RemoteDashboardControlPolicyTests: XCTestCase {
    private func focus(_ active: Bool, connected: Bool = true, selection: String? = "screen", immersive: Bool = true) -> RemoteDashboardFocus {
        .init(immersive: immersive, active: active, connected: connected, selection: selection)
    }
    func testDeactivationReleasesLeaseAndSameViewerReacquiresWithoutExiting() {
        var policy = RemoteDashboardControlPolicy()
        XCTAssertEqual(policy.update(focus(true)), .acquire)
        XCTAssertEqual(policy.update(focus(false)), .release)
        XCTAssertTrue(policy.previous.immersive)
        XCTAssertTrue(policy.wantsControl)
        XCTAssertEqual(policy.update(focus(true)), .acquire)
        XCTAssertEqual(policy.update(focus(true)), .none)
        XCTAssertEqual(policy.update(focus(true, immersive: false)), .release)
        XCTAssertFalse(policy.wantsControl)
    }
    func testLateBackgroundConnectionWaitsForExistingForegroundIntent() {
        var policy = RemoteDashboardControlPolicy()
        XCTAssertEqual(policy.update(focus(true, connected: false)), .none)
        XCTAssertEqual(policy.update(focus(false, connected: false)), .release)
        XCTAssertEqual(policy.update(focus(false)), .none)
        XCTAssertEqual(policy.update(focus(true)), .acquire)
    }
    func testConnectionCreatedInBackgroundWithoutIntentCannotAcquireOnForeground() {
        var policy = RemoteDashboardControlPolicy()
        XCTAssertEqual(policy.update(focus(false)), .release)
        XCTAssertFalse(policy.wantsControl)
        XCTAssertEqual(policy.update(focus(true)), .none)
    }
    func testExplicitExitCancelsDeferredAcquisition() {
        var policy = RemoteDashboardControlPolicy()
        _ = policy.update(focus(true))
        _ = policy.update(focus(false))
        XCTAssertEqual(policy.update(focus(false, immersive: false)), .release)
        XCTAssertEqual(policy.update(focus(true, immersive: false)), .none)
    }
    func testDifferentViewerCannotInheritBackgroundIntent() {
        var policy = RemoteDashboardControlPolicy()
        _ = policy.update(focus(true))
        _ = policy.update(focus(false))
        XCTAssertEqual(policy.update(focus(false, selection: "another")), .none)
        XCTAssertFalse(policy.wantsControl)
        XCTAssertEqual(policy.update(focus(true, selection: "another")), .none)
    }
    func testDeniedOrRevokedIntentCannotLoopOnActivationOrReconnect() {
        var policy = RemoteDashboardControlPolicy()
        _ = policy.update(focus(true))
        policy.clearIntent()
        _ = policy.update(focus(false))
        XCTAssertEqual(policy.update(focus(true)), .none)
        _ = policy.update(focus(true, connected: false))
        XCTAssertEqual(policy.update(focus(true)), .none)
        XCTAssertEqual(policy.update(focus(true, selection: "new-publication-generation")), .none,
                       "A new automatic publication cannot erase denial/revocation")
        policy.requestControl() // Explicit Take control may establish new intent.
        _ = policy.update(focus(false, selection: "new-publication-generation"))
        XCTAssertEqual(policy.update(focus(true, selection: "new-publication-generation")), .acquire)
    }
    func testHiddenRetainedDashboardCannotAcquireAfterReconnect() {
        var policy = RemoteDashboardControlPolicy()
        _ = policy.update(focus(true))
        _ = policy.update(focus(true, immersive: false))
        _ = policy.update(focus(true, connected: false, immersive: false))
        XCTAssertEqual(policy.update(focus(true, immersive: false)), .none)
    }
    func testFirstSelectionMayCompleteAlreadyRequestedForegroundIntent() {
        var policy = RemoteDashboardControlPolicy()
        _ = policy.update(focus(true, connected: false, selection: nil))
        _ = policy.update(focus(false, connected: false, selection: nil))
        XCTAssertEqual(policy.update(focus(false)), .none)
        XCTAssertEqual(policy.update(focus(true)), .acquire)
    }
    func testNonimmersiveViewerReleasesOnDeactivation() {
        var policy = RemoteDashboardControlPolicy()
        _ = policy.update(focus(true, immersive: false))
        XCTAssertEqual(policy.update(focus(false, immersive: false)), .release)
        XCTAssertEqual(policy.update(focus(true, immersive: false)), .none)
    }
}
