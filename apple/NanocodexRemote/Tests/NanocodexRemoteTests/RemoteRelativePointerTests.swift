import XCTest
@testable import NanocodexRemote

final class RemoteRelativePointerTests: XCTestCase {
    func testCapabilityIsExplicitAndOlderHostsRemainAbsolute() throws {
        let decoder = JSONDecoder()
        let old = try decoder.decode(RemoteControlMessage.self, from: Data(#"{"type":"granted","generation":"g"}"#.utf8))
        let new = try decoder.decode(RemoteControlMessage.self, from: Data(#"{"type":"granted","generation":"g","relativePointer":true}"#.utf8))
        XCTAssertNil(old.relativePointer)
        XCTAssertEqual(new.relativePointer, true)
    }
    func testRelativeDragAndBothHeldButtonsRoundTripWithoutAbsoluteWarp() throws {
        let events: [RemoteInput] = [
            .init(kind: .button, sequence: 1, generation: "g", button: 0, down: true),
            .init(kind: .button, sequence: 2, generation: "g", button: 1, down: true),
            .init(kind: .relativeMove, sequence: 3, generation: "g", deltaX: -12.5, deltaY: 4),
            .init(kind: .button, sequence: 4, generation: "g", button: 0, down: false),
            .init(kind: .relativeMove, sequence: 5, generation: "g", deltaX: 10, deltaY: -4),
            .init(kind: .button, sequence: 6, generation: "g", button: 1, down: false),
        ]
        for event in events {
            XCTAssertEqual(try RemoteInput.decode(JSONEncoder().encode(event)), event)
            XCTAssertNil(event.x); XCTAssertNil(event.y)
        }
    }
    func testRelativeMotionRejectsInvalidOrAmbiguousCoordinates() {
        for event in [
            RemoteInput(kind: .relativeMove, sequence: 1, generation: "g", deltaX: .nan, deltaY: 0),
            RemoteInput(kind: .relativeMove, sequence: 1, generation: "g", deltaX: 4097, deltaY: 0),
            RemoteInput(kind: .relativeMove, sequence: 1, generation: "g", x: 0, y: 0, deltaX: 1, deltaY: 0),
            RemoteInput(kind: .button, sequence: 1, generation: "g", x: 0, button: 0, down: true),
        ] { XCTAssertThrowsError(try event.validate()) }
    }
}
