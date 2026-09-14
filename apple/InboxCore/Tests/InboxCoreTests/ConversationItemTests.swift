import XCTest
@testable import InboxCore

final class ConversationItemTests: XCTestCase {
    func testQueuedSteeringMessageDoesNotAppearToBeExecuting() {
        var original = TranscriptRow(id: "original-message", role: "You", text: "Start work")
        original.turnID = "original"
        var wait = TranscriptRow(id: "wait", role: "Tool", text: "Wait", running: true)
        wait.turnID = "original"
        var correction = TranscriptRow(id: "correction-message", role: "You", text: "Show each desktop")
        correction.turnID = "correction"
        let rows = [original, wait, correction]

        let queued = ConversationItem.group(rows, activeTurns: ["original", "correction"])
        XCTAssertEqual(queued.compactMap(\.message).map(\.text), ["Start work", "Show each desktop"])
        XCTAssertEqual(queued.filter(\.isRunning).map(\.id), ["activity-original"])
        XCTAssertFalse(queued.contains { $0.id == "activity-correction" })

        // Completion or an acknowledged steer advances the durable queue.
        let started = ConversationItem.group(rows, activeTurns: ["correction"])
        XCTAssertEqual(started.filter(\.isRunning).map(\.id), ["activity-correction"])
        XCTAssertEqual(started.first { $0.id == "activity-original" }?.activity.map(\.id), ["wait"])
        XCTAssertFalse(ConversationItem.group(rows).contains(where: \.isRunning))
    }

    func testQueueHeadOutsideHistoryDoesNotPromoteVisibleFollowUp() {
        var correction = TranscriptRow(id: "message", role: "You", text: "Next request")
        correction.turnID = "queued"
        let items = ConversationItem.group([correction], activeTurns: ["outside-window", "queued"])
        XCTAssertEqual(items.count, 1)
        XCTAssertFalse(items.contains(where: \.isRunning))
    }
}
