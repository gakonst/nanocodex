#if os(macOS)
import XCTest
@testable import NanocodexRemote

final class MacCapturedInputTests: XCTestCase {
    func testOppositeModifiersRemainIndependentOnRelease() {
        // Aggregate flag stays set while the opposite-side key is held.
        let pairs: [(UInt16, UInt16, UInt, UInt, UInt)] = [
            (224, 228, 0x1, 0x2000, 1 << 18),
            (225, 229, 0x2, 0x4, 1 << 17),
            (226, 230, 0x20, 0x40, 1 << 19),
            (227, 231, 0x8, 0x10, 1 << 20)
        ]
        for (left, right, leftMask, rightMask, aggregate) in pairs {
            XCTAssertEqual(MacCapturedInputPolicy.modifierDown(key: left, flags: aggregate | leftMask | rightMask), true)
            XCTAssertEqual(MacCapturedInputPolicy.modifierDown(key: right, flags: aggregate | leftMask | rightMask), true)
            XCTAssertEqual(MacCapturedInputPolicy.modifierDown(key: left, flags: aggregate | rightMask), false)
            XCTAssertEqual(MacCapturedInputPolicy.modifierDown(key: right, flags: aggregate | rightMask), true)
            XCTAssertEqual(MacCapturedInputPolicy.modifierDown(key: right, flags: aggregate | leftMask), false)
        }
        XCTAssertNil(MacCapturedInputPolicy.modifierDown(key: 4, flags: 0))
    }

    func testCapturedPhysicalDownSuppressesRepeatAndDuplicateDown() {
        XCTAssertTrue(MacCapturedInputPolicy.sendsPhysicalDown(isRepeat: false, alreadyPressed: false))
        XCTAssertFalse(MacCapturedInputPolicy.sendsPhysicalDown(isRepeat: true, alreadyPressed: true))
        XCTAssertFalse(MacCapturedInputPolicy.sendsPhysicalDown(isRepeat: true, alreadyPressed: false))
        XCTAssertFalse(MacCapturedInputPolicy.sendsPhysicalDown(isRepeat: false, alreadyPressed: true))
    }

    func testRelativeMotionBoundsAndNonfiniteValues() {
        XCTAssertEqual(MacCapturedInputPolicy.boundedDelta(12.5), 12.5)
        XCTAssertEqual(MacCapturedInputPolicy.boundedDelta(5000), 4096)
        XCTAssertEqual(MacCapturedInputPolicy.boundedDelta(-5000), -4096)
        XCTAssertEqual(MacCapturedInputPolicy.boundedDelta(.nan), 0)
        XCTAssertEqual(MacCapturedInputPolicy.boundedDelta(.infinity), 0)
    }
}
#endif
