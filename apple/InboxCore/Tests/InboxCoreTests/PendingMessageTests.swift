import XCTest
@testable import InboxCore

final class PendingMessageTests: XCTestCase {
    func testRetryAndRestoreKeepSubmissionIdentity() throws {
        let original = PendingMessage(agentID: "agent-a", input: "Keep this once", predecessor: "old", id: "queued")
        var restored = try JSONDecoder().decode(PendingMessage.self, from: JSONEncoder().encode(original))
        restored.restore()
        XCTAssertEqual(restored.phase, .failed)
        XCTAssertEqual(try restored.submission.requestSpec().key, "inbox:queued")
        XCTAssertEqual(try restored.submission.requestSpec().body?["id"].string, "queued")
        XCTAssertEqual(try restored.submission.requestSpec().body?["input"].string, original.input)
    }
    func testForceOnlyCancelsCapturedPredecessor() throws {
        var message = PendingMessage(agentID: "agent-a", input: "New direction", predecessor: "old", id: "queued")
        XCTAssertNil(message.interruption)
        message.phase = .queued
        let spec = try XCTUnwrap(message.interruption).requestSpec()
        XCTAssertEqual(spec.path, "/v1/agents/agent-a/turns/old/cancel")
        XCTAssertNil(spec.body)
        message.phase = .starting
        XCTAssertNil(message.interruption, "Repeated force taps cannot start another cancellation")
        message.restore()
        XCTAssertNotNil(message.interruption)
        var idle = PendingMessage(agentID: "a", input: "x", predecessor: "")
        idle.phase = .queued; XCTAssertNil(idle.interruption)
        var selfTarget = PendingMessage(agentID: "a", input: "x", predecessor: "same", id: "same")
        selfTarget.phase = .queued; XCTAssertNil(selfTarget.interruption)
    }
    func testStalePollCannotLoseQueuedMessage() {
        var message = PendingMessage(agentID: "a", input: "x", predecessor: "old", id: "queued")
        XCTAssertFalse(message.hasFinished(activeTurns: [], stateCursor: Cursor(rawValue: "99")!))
        message.acceptedCursor = Cursor(rawValue: "20")
        XCTAssertFalse(message.hasFinished(activeTurns: [], stateCursor: Cursor(rawValue: "19")!))
        XCTAssertFalse(message.hasFinished(activeTurns: ["queued"], stateCursor: Cursor(rawValue: "21")!))
        XCTAssertTrue(message.hasFinished(activeTurns: [], stateCursor: Cursor(rawValue: "21")!))
    }
    func testOnlyOwnExecutionOrTerminalEventClearsPending() throws {
        let message = PendingMessage(agentID: "a", input: "x", predecessor: "old", id: "queued")
        func event(_ turn: String, _ type: String, inner: String = "") throws -> AgentEvent {
            try AgentEvent(.object(["cursor": .string("42"), "turn_id": .string(turn), "type": .string(type), "event": .object(["type": .string(inner)])]))
        }
        XCTAssertFalse(message.hasStarted(in: [try event("queued", "turn_accepted")]))
        XCTAssertFalse(message.hasStarted(in: [try event("old", "turn_cancelled")]))
        XCTAssertFalse(message.hasStarted(in: [try event("other", "event", inner: "run.started")]))
        XCTAssertTrue(message.hasStarted(in: [try event("queued", "event", inner: "run.started")]))
        for terminal in ["turn_completed", "turn_cancelled", "turn_failed"] {
            XCTAssertTrue(message.hasStarted(in: [try event("queued", terminal)]))
        }
    }
}
