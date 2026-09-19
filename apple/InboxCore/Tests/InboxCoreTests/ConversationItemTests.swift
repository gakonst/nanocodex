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
        XCTAssertEqual(queued.filter(\.isRunning).map(\.id), ["wait"])
        XCTAssertFalse(queued.contains { $0.id == "activity-correction" })

        // Completion or an acknowledged steer advances the durable queue.
        let started = ConversationItem.group(rows, activeTurns: ["correction"])
        XCTAssertTrue(started.filter(\.isRunning).isEmpty)
        XCTAssertEqual(started.first { $0.id == "wait" }?.activity.map(\.id), ["wait"])
        XCTAssertFalse(ConversationItem.group(rows).contains(where: \.isRunning))
    }

    func testInterleavedTurnsPreserveSourceOrderAndStableToolIdentity() {
        var tool = TranscriptRow(id: "call", role: "Tool", text: "Read", running: true)
        tool.turnID = "first"
        var followup = TranscriptRow(id: "followup", role: "You", text: "Also check this")
        followup.turnID = "second"
        var update = TranscriptRow(id: "update", role: "Agent", text: "Found it")
        update.turnID = "first"
        update.phase = "commentary"
        let initial = ConversationItem.group([tool, followup, update], activeTurns: ["first"])
        tool.running = false
        let completed = ConversationItem.group([tool, followup, update])
        XCTAssertEqual(initial.map(\.id), ["call", "followup", "update"])
        XCTAssertEqual(completed.map(\.id), initial.map(\.id))
        XCTAssertEqual(completed.first?.activity.map(\.id), ["call"])
        XCTAssertEqual(completed.last?.message?.text, "Found it")
    }

    func testQueueHeadOutsideHistoryDoesNotPromoteVisibleFollowUp() {
        var correction = TranscriptRow(id: "message", role: "You", text: "Next request")
        correction.turnID = "queued"
        let items = ConversationItem.group([correction], activeTurns: ["outside-window", "queued"])
        XCTAssertEqual(items.count, 1)
        XCTAssertFalse(items.contains(where: \.isRunning))
    }
}
