import XCTest
@testable import NanocodexRemote

final class RemoteGameInputStateTests: XCTestCase {
    func testSharedKeyReleasedOnlyAfterLastOwner() {
        var state = RemoteGameInputState()
        XCTAssertEqual(state.setKeys([26], owner: "stick"), [.key(26, true)])
        XCTAssertEqual(state.setKeys([26], owner: "other"), [])
        XCTAssertEqual(state.setKeys([], owner: "stick"), [])
        XCTAssertEqual(state.setKeys([], owner: "other"), [.key(26, false)])
        XCTAssertTrue(state.keys.isEmpty)
    }

    func testDiagonalChangesReleaseBeforePressWithoutRepeats() {
        var state = RemoteGameInputState()
        XCTAssertEqual(state.setKeys([4, 26], owner: "stick"), [.key(4, true), .key(26, true)])
        XCTAssertEqual(state.setKeys([4, 26], owner: "stick"), [])
        XCTAssertEqual(state.setKeys([7, 26], owner: "stick"), [.key(4, false), .key(7, true)])
        XCTAssertEqual(state.setKeys([225], owner: "shift"), [.key(225, true)])
        XCTAssertEqual(state.setKeys([], owner: "stick"), [.key(7, false), .key(26, false)])
        XCTAssertEqual(state.keys, [225])
    }

    func testStopClearsKeysAndCameraAndLateReleasesAreHarmless() {
        var state = RemoteGameInputState()
        _ = state.setKeys([26, 44, 224], owner: "touches")
        XCTAssertEqual(state.camera(true), [.camera(true)])
        XCTAssertEqual(state.camera(true), [])
        XCTAssertEqual(state.reset(), [.releaseAll])
        XCTAssertTrue(state.owners.isEmpty)
        XCTAssertFalse(state.cameraHeld)
        XCTAssertEqual(state.setKeys([], owner: "touches"), [])
        XCTAssertEqual(state.camera(false), [])
        XCTAssertEqual(state.setKeys([44], owner: "new"), [.key(44, true)])
    }

    func testJoystickDeadZoneDiagonalsAndInvalidCoordinates() {
        XCTAssertEqual(RemoteGameInputState.movement(x: 18, y: -18), [])
        XCTAssertEqual(RemoteGameInputState.movement(x: -40, y: -40), [4, 26])
        XCTAssertEqual(RemoteGameInputState.movement(x: 40, y: 40), [7, 22])
        XCTAssertEqual(RemoteGameInputState.movement(x: .nan, y: 40), [])
        XCTAssertEqual(RemoteGameInputState.movement(x: 40, y: .infinity), [])
    }
}
