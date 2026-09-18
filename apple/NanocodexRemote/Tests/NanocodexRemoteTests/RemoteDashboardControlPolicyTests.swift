import XCTest
@testable import NanocodexRemote

final class RemoteDashboardControlPolicyTests: XCTestCase {
    func testBackgroundDoesNotExitImmersionOrReleaseItsLease() {
        var policy = RemoteDashboardControlPolicy()
        XCTAssertEqual(policy.update(.init(immersive: true, active: true, connected: true)), .acquire)
        XCTAssertEqual(policy.update(.init(immersive: true, active: false, connected: true)), .none)
        XCTAssertEqual(policy.update(.init(immersive: true, active: true, connected: true)), .none)
        XCTAssertEqual(policy.update(.init(immersive: false, active: true, connected: true)), .release)
        XCTAssertEqual(policy.update(.init(immersive: false, active: true, connected: true)), .none)
    }

    func testLateBackgroundConnectionWaitsForActivation() {
        var policy = RemoteDashboardControlPolicy()
        XCTAssertEqual(policy.update(.init(immersive: true, active: true, connected: false)), .none)
        XCTAssertEqual(policy.update(.init(immersive: true, active: false, connected: true)), .none)
        XCTAssertEqual(policy.update(.init(immersive: true, active: true, connected: true)), .acquire)
    }

    func testExplicitExitCancelsDeferredAcquisition() {
        var policy = RemoteDashboardControlPolicy()
        _ = policy.update(.init(immersive: true, active: false, connected: true))
        XCTAssertEqual(policy.update(.init(immersive: false, active: false, connected: true)), .release)
        XCTAssertEqual(policy.update(.init(immersive: false, active: true, connected: true)), .none)
    }

    func testHiddenRetainedDashboardCannotAcquireAfterReconnect() {
        var policy = RemoteDashboardControlPolicy()
        _ = policy.update(.init(immersive: true, active: true, connected: true))
        _ = policy.update(.init(immersive: false, active: true, connected: true))
        _ = policy.update(.init(immersive: false, active: true, connected: false))
        XCTAssertEqual(policy.update(.init(immersive: false, active: true, connected: true)), .none)
    }

    func testHostRevocationDoesNotRetakeOnActivation() throws {
        var policy = RemoteDashboardControlPolicy()
        var control = RemoteViewerControl()
        XCTAssertEqual(policy.update(.init(immersive: true, active: true, connected: true)), .acquire)
        _ = control.acquire()
        _ = try control.receive(.init(type: .granted, generation: "immersive"))
        _ = try control.receive(.init(type: .revoked, generation: "immersive"))
        XCTAssertNil(control.generation)
        XCTAssertFalse(control.requested)
        XCTAssertEqual(policy.update(.init(immersive: true, active: false, connected: true)), .none)
        XCTAssertEqual(policy.update(.init(immersive: true, active: true, connected: true)), .none)
    }

    func testNonimmersiveViewerStillReleasesOnBackground() {
        var policy = RemoteDashboardControlPolicy()
        _ = policy.update(.init(immersive: false, active: true, connected: true))
        XCTAssertEqual(policy.update(.init(immersive: false, active: false, connected: true)), .release)
    }
}
