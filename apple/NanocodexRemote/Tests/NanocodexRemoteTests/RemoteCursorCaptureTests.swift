#if os(macOS)
import XCTest
@testable import NanocodexRemote

final class RemoteCursorCaptureTests: XCTestCase {
    func testRepeatedFramesDoNotAccumulateHideCountAndFocusLossRestoresCursor() {
        var balance = 0
        let cursor = RemoteCursorCapture(hide: { balance += 1 }, unhide: { balance -= 1 })
        for _ in 0..<100 { cursor.update(hidden: true) }
        XCTAssertEqual(balance, 1)
        cursor.update(hidden: false)
        cursor.update(hidden: false)
        XCTAssertEqual(balance, 0)
        cursor.update(hidden: true)
        XCTAssertEqual(balance, 1)
        cursor.update(hidden: false)
        XCTAssertEqual(balance, 0)
    }

    func testDestroyingActiveCaptureRestoresCursor() {
        var balance = 0
        var cursor: RemoteCursorCapture? = RemoteCursorCapture(hide: { balance += 1 }, unhide: { balance -= 1 })
        cursor?.update(hidden: true)
        cursor = nil
        XCTAssertEqual(balance, 0)
    }
}
#endif
