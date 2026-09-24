import XCTest
@testable import NanocodexRemote

final class RemoteNativeGameInputStateTests: XCTestCase {
    func testNativePresentationSurvivesLostLeaseOnlyForSameHand() {
        XCTAssertTrue(RemoteNativeGameContext.presentsNative(capable: true, controlling: true, handID: "one", rememberedHandID: nil))
        XCTAssertTrue(RemoteNativeGameContext.presentsNative(capable: false, controlling: false, handID: "one", rememberedHandID: "one"))
        XCTAssertFalse(RemoteNativeGameContext.presentsNative(capable: false, controlling: true, handID: "one", rememberedHandID: "one"))
        XCTAssertFalse(RemoteNativeGameContext.presentsNative(capable: false, controlling: false, handID: "two", rememberedHandID: "one"))
        XCTAssertFalse(RemoteNativeGameContext.presentsNative(capable: false, controlling: false, handID: nil, rememberedHandID: nil))
    }

    func testIndependentOwnersAndNeutralReset() {
        var state = RemoteNativeGameInputState()
        state.button("a", owner: "finger1", down: true)
        state.button("a", owner: "finger2", down: true)
        state.button("leftShoulder", owner: "finger3", down: true)
        state.button("a", owner: "finger1", down: false)
        XCTAssertEqual(state.buttons, ["a", "leftShoulder"])
        state.trigger("leftTrigger", owner: "finger4", down: true)
        state.trigger("leftTrigger", owner: "accessibility", down: true)
        state.trigger("leftTrigger", owner: "accessibility", down: false)
        XCTAssertEqual(state.triggers, ["leftTrigger"])
        state.stick("left", x: 0.8, y: 0.2)
        state.stick("right", x: -0.3, y: 0.5)
        state.stick("left", x: 0, y: 0)
        XCTAssertNil(state.sticks["left"])
        XCTAssertNotNil(state.sticks["right"])
        state.reset()
        XCTAssertEqual(state.buttons, [])
        XCTAssertTrue(state.triggers.isEmpty)
        XCTAssertTrue(state.sticks.isEmpty)
        XCTAssertTrue(state.buttonsByOwner.isEmpty)
        XCTAssertTrue(state.triggersByOwner.isEmpty)
        state.button("a", owner: "finger2", down: false)
        XCTAssertEqual(state.buttons, [])
    }

    func testAnalogClampDeadZoneAndInvalidValues() {
        var state = RemoteNativeGameInputState()
        state.stick("left", x: 10, y: 10)
        let point = state.sticks["left"]!
        XCTAssertEqual(hypot(point.x, point.y), 1, accuracy: 0.000001)
        XCTAssertGreaterThan(point.y, 0) // positive Y means down, matching the wire protocol
        state.stick("left", x: 0.05, y: 0.05)
        XCTAssertNil(state.sticks["left"])
        state.stick("right", x: .nan, y: 1)
        XCTAssertNil(state.sticks["right"])
        state.stick("right", x: 1, y: .infinity)
        XCTAssertNil(state.sticks["right"])
    }

    func testFullSnapshotPreservesOtherControlsOnReleaseAndResetsToNeutral() throws {
        var state = RemoteNativeGameInputState()
        XCTAssertTrue(state.isNeutral)
        XCTAssertEqual(state.snapshot, RemoteGamepadState())
        state.stick("left", x: 1, y: 0)
        state.stick("right", x: 0, y: -1)
        state.trigger("rightTrigger", down: true)
        for button in ["a", "b", "x", "y", "dpadUp", "dpadDown", "dpadLeft", "dpadRight",
                       "leftShoulder", "rightShoulder", "leftStick", "rightStick", "back", "start"] {
            state.button(button, owner: button, down: true)
        }
        try state.snapshot.validate()
        XCTAssertFalse(state.isNeutral)
        XCTAssertEqual(state.snapshot.buttons.count, 14)
        XCTAssertEqual(state.snapshot.leftX, 1)
        XCTAssertEqual(state.snapshot.rightY, -1)
        XCTAssertEqual(state.snapshot.leftTrigger, 0)
        XCTAssertEqual(state.snapshot.rightTrigger, 1)
        state.button("a", owner: "a", down: false)
        state.stick("left", x: 0, y: 0)
        XCTAssertFalse(state.snapshot.buttons.contains("a"))
        XCTAssertEqual(state.snapshot.buttons.count, 13)
        XCTAssertEqual(state.snapshot.leftX, 0)
        XCTAssertEqual(state.snapshot.rightY, -1)
        XCTAssertEqual(state.snapshot.rightTrigger, 1)
        state.reset()
        XCTAssertTrue(state.isNeutral)
        XCTAssertEqual(state.snapshot, RemoteGamepadState())
    }

    func testTriggerOwnersPreserveFullStateUntilFinalRelease() throws {
        var state = RemoteNativeGameInputState()
        state.trigger("leftTrigger", owner: "touch", down: true)
        state.trigger("leftTrigger", owner: "accessibility", down: true)
        state.trigger("rightTrigger", owner: "otherTouch", down: true)
        state.button("b", owner: "face", down: true)
        state.stick("right", x: 1, y: 0)
        state.trigger("leftTrigger", owner: "accessibility", down: false)
        state.trigger("leftTrigger", owner: "unknown", down: false)
        XCTAssertEqual(state.snapshot.leftTrigger, 1)
        state.trigger("leftTrigger", owner: "touch", down: false)
        XCTAssertEqual(state.snapshot.leftTrigger, 0)
        XCTAssertEqual(state.snapshot.rightTrigger, 1)
        XCTAssertEqual(state.snapshot.buttons, ["b"])
        XCTAssertEqual(state.snapshot.rightX, 1)
        try state.snapshot.validate()
        state.reset()
        state.trigger("rightTrigger", owner: "otherTouch", down: false)
        XCTAssertTrue(state.isNeutral)
        XCTAssertTrue(state.triggersByOwner.isEmpty)
    }
}
