import XCTest
@testable import InboxCore

final class MeetingSegmentPolicyTests: XCTestCase {
    func testOutOfOrderResultsPreserveCaptureOrderAndReleaseSettledSegments() {
        var policy = MeetingSegmentPolicy()
        let first = policy.begin(), second = policy.begin(), third = policy.begin()
        policy.update(third, text: "third")
        policy.update(first, text: "first")
        policy.update(second, text: "second")
        policy.settle(third)
        policy.settle(first)
        policy.settle(second)
        XCTAssertEqual(policy.transcript, "first\nsecond\nthird")
        XCTAssertEqual(policy.unfinished, 0)
        policy.update(first, text: "late partial")
        XCTAssertEqual(policy.transcript, "first\nsecond\nthird")
    }

    func testRepeatedRotationsKeepOnlyTextWhileCappingUnfinishedRequests() {
        var policy = MeetingSegmentPolicy()
        for index in 0..<300 {
            let segment = policy.begin()
            policy.update(segment, text: "part \(index)")
            policy.settle(segment)
        }
        XCTAssertEqual(policy.unfinished, 0)
        XCTAssertEqual(policy.transcript.components(separatedBy: "\n").count, 300)
        XCTAssertTrue(policy.canRotate(sealedPending: 2))
        XCTAssertFalse(policy.canRotate(sealedPending: 3))
    }
}
