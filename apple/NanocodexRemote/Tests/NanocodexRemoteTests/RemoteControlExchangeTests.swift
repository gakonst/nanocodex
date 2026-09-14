import XCTest
@testable import NanocodexRemote

final class RemoteControlExchangeTests: XCTestCase {
    // Both existing Linux and native hosts send this acknowledgement without a
    // generation. The ordered reliable channel must finish it before reacquire.
    private func revoked() throws -> RemoteControlMessage {
        try JSONDecoder().decode(RemoteControlMessage.self, from: Data(#"{"type":"revoked"}"#.utf8))
    }

    func testImmediateRetakeWaitsForReleaseAcknowledgement() throws {
        var viewer = RemoteViewerControl()
        XCTAssertEqual(viewer.acquire()?.type, .acquire)
        XCTAssertNil(try viewer.receive(.init(type: .granted, generation: "first")))
        let release = try XCTUnwrap(viewer.release())
        XCTAssertEqual(release.type, .release)
        XCTAssertEqual(release.generation, "first")
        XCTAssertNil(viewer.generation)
        XCTAssertNil(viewer.acquire(), "The old release acknowledgement must not overlap the next acquire")
        XCTAssertEqual(try viewer.receive(revoked())?.type, .acquire)
        XCTAssertTrue(viewer.requested)
        XCTAssertNil(try viewer.receive(.init(type: .granted, generation: "second")))
        XCTAssertEqual(viewer.generation, "second")
    }

    func testCancelledOutstandingGrantIsReleasedBeforeExplicitRetake() throws {
        var viewer = RemoteViewerControl()
        _ = viewer.acquire()
        XCTAssertNil(viewer.release())
        XCTAssertNil(viewer.acquire())
        let release = try XCTUnwrap(viewer.receive(.init(type: .granted, generation: "cancelled")))
        XCTAssertEqual(release.type, .release)
        XCTAssertEqual(release.generation, "cancelled")
        XCTAssertNil(viewer.generation, "A cancelled grant must never enable input")
        XCTAssertEqual(try viewer.receive(revoked())?.type, .acquire)
        _ = try viewer.receive(.init(type: .granted, generation: "retaken"))
        XCTAssertEqual(viewer.generation, "retaken")
    }

    func testActualRevocationNeverReacquiresOrEchoesRelease() throws {
        var viewer = RemoteViewerControl()
        _ = viewer.acquire()
        _ = try viewer.receive(.init(type: .granted, generation: "held"))
        XCTAssertNil(try viewer.receive(revoked()))
        XCTAssertNil(viewer.generation)
        XCTAssertFalse(viewer.requested)
        XCTAssertNil(try viewer.receive(revoked()))
        XCTAssertEqual(viewer.acquire()?.type, .acquire, "Only another explicit take can request control")
    }

    func testCancellingRetakeWhileReleaseIsPendingDoesNotAcquire() throws {
        var viewer = RemoteViewerControl()
        _ = viewer.acquire()
        _ = try viewer.receive(.init(type: .granted, generation: "first"))
        _ = viewer.release()
        _ = viewer.acquire()
        XCTAssertNil(viewer.release())
        XCTAssertNil(try viewer.receive(revoked()))
        XCTAssertFalse(viewer.requested)
        XCTAssertNil(viewer.generation)
    }

    func testStaleGenerationRevocationCannotClearCurrentControl() throws {
        var viewer = RemoteViewerControl()
        _ = viewer.acquire()
        _ = try viewer.receive(.init(type: .granted, generation: "current"))
        XCTAssertNil(try viewer.receive(.init(type: .revoked, generation: "old")))
        XCTAssertEqual(viewer.generation, "current")
        _ = viewer.release()
        _ = viewer.acquire()
        XCTAssertNil(try viewer.receive(.init(type: .revoked, generation: "old")))
        XCTAssertEqual(try viewer.receive(.init(type: .revoked, generation: "current"))?.type, .acquire)
    }

    func testDenialAfterCancelledAcquireHonorsOnlyTheExplicitRetake() throws {
        var viewer = RemoteViewerControl()
        _ = viewer.acquire()
        _ = viewer.release()
        _ = viewer.acquire()
        XCTAssertEqual(try viewer.receive(.init(type: .denied))?.type, .acquire)
        XCTAssertNil(try viewer.receive(.init(type: .denied)))
        XCTAssertFalse(viewer.requested)
        XCTAssertNil(viewer.generation)
        XCTAssertThrowsError(try viewer.receive(.init(type: .granted, generation: "unsolicited")))
    }
}
