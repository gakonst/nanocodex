import XCTest
@testable import InboxCore

final class HandTaskProgressTests: XCTestCase {
    func testOnlyFreshActivityForTheOwnedTurnAdvancesProgress() throws {
        var progress = HandTaskProgress(turnID: "owned")
        func event(_ cursor: String, turn: String = "owned", type: String = "tool.result") throws -> AgentEvent {
            try AgentEvent(.object(["type": .string("event"), "turn_id": .string(turn),
                                   "event": .object(["type": .string(type)])]), cursor: cursor)
        }
        XCTAssertTrue(progress.receive(try event("9007199254740993")))
        XCTAssertEqual(progress.completedUnits, 1)
        XCTAssertFalse(progress.receive(try event("9007199254740993")))
        XCTAssertFalse(progress.receive(try event("9007199254740992")))
        XCTAssertFalse(progress.receive(try event("9007199254740994", turn: "other")))
        XCTAssertFalse(progress.receive(try event("9007199254740995", type: "heartbeat")))
        XCTAssertEqual(progress.completedUnits, 1)
        XCTAssertTrue(progress.receive(try event("9007199254740996", type: "assistant.delta")))
        XCTAssertEqual(progress.completedUnits, 2)
        XCTAssertEqual(progress.detail, "Writing response")
    }
    func testReceiptCompletionUpdatesStaleActivityWithoutInventingProgress() throws {
        var progress = HandTaskProgress(turnID: "owned")
        let writing = try AgentEvent(.object(["type": .string("event"), "turn_id": .string("owned"),
            "event": .object(["type": .string("assistant.delta")])]), cursor: "1")
        XCTAssertTrue(progress.receive(writing))
        XCTAssertEqual(progress.detail, "Writing response")
        let units = progress.completedUnits
        progress.finish(.completed)
        XCTAssertEqual(progress.detail, "Completed")
        XCTAssertEqual(progress.completedUnits, units)
        let late = try AgentEvent(writing.data, cursor: "2")
        XCTAssertFalse(progress.receive(late), "Late stream activity cannot revive a finished task")
        XCTAssertEqual(progress.detail, "Completed")
        XCTAssertEqual(progress.cursor, Cursor(rawValue: "2"))
    }

    func testFailureStopAndSuspensionHaveDistinctFinalPresentation() {
        for outcome in [HandTaskOutcome.failed, .stopped, .paused] {
            var progress = HandTaskProgress(turnID: "owned")
            progress.finish(outcome)
            XCTAssertEqual(progress.detail, outcome.rawValue)
            XCTAssertEqual(progress.completedUnits, 0, "Runtime outcomes do not pretend agent work happened")
            progress.finish(.completed)
            XCTAssertEqual(progress.outcome, outcome, "Cleanup must not overwrite the recorded outcome")
        }
    }

    func testTerminalEventPresentationSurvivesLaterCleanupAndOtherTurns() throws {
        for (type, outcome) in [("turn_completed", HandTaskOutcome.completed), ("turn_failed", .failed), ("turn_cancelled", .stopped)] {
            var progress = HandTaskProgress(turnID: "owned")
            let other = try AgentEvent(.object(["type": .string(type), "turn_id": .string("other")]), cursor: "1")
            XCTAssertFalse(progress.receive(other))
            XCTAssertNil(progress.outcome)
            let terminal = try AgentEvent(.object(["type": .string(type), "turn_id": .string("owned")]), cursor: "2")
            XCTAssertTrue(progress.receive(terminal))
            XCTAssertEqual(progress.outcome, outcome)
            XCTAssertEqual(progress.completedUnits, 1)
            progress.finish(.failed)
            XCTAssertEqual(progress.outcome, outcome)
            XCTAssertFalse(progress.receive(terminal))
        }
    }

}
