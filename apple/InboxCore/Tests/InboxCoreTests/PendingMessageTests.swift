import XCTest
@testable import InboxCore

final class PendingMessageTests: XCTestCase {
    func testAttachmentReferencesSurviveRetryWithoutEmbeddingImageBytes() throws {
        let attachment = try MessageAttachment(name: "Image.jpg", byteCount: 4096)
        let message = PendingMessage(agentID: "a", input: "Describe this", predecessor: "previous", id: "same-turn", attachments: [attachment])
        let saved = try JSONEncoder().encode(message)
        XCTAssertLessThan(saved.count, 1024)
        var restored = try JSONDecoder().decode(PendingMessage.self, from: saved)
        restored.restore()
        XCTAssertEqual(restored.attachments, [attachment])
        XCTAssertEqual(restored.submission.requestID, "same-turn")
        XCTAssertEqual(restored.agentID, "a")
        let legacy = Data(#"{"id":"legacy","agentID":"a","input":"Hello","predecessor":"","phase":"queued"}"#.utf8)
        XCTAssertNil(try JSONDecoder().decode(PendingMessage.self, from: legacy).attachments)
    }
    func testRetryAndRestoreKeepSubmissionIdentity() throws {
        let original = PendingMessage(agentID: "agent-a", input: "Keep this once", predecessor: "old", id: "queued", contextIDs: ["capture-a"])
        var restored = try JSONDecoder().decode(PendingMessage.self, from: JSONEncoder().encode(original))
        restored.restore()
        XCTAssertEqual(restored.phase, .failed)
        XCTAssertEqual(try restored.submission.requestSpec().key, "inbox:queued")
        XCTAssertEqual(try restored.submission.requestSpec().body?["id"].string, "queued")
        XCTAssertEqual(try restored.submission.requestSpec().body?["input"].string, original.input)
        XCTAssertEqual(restored.contextIDs, ["capture-a"])
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
        XCTAssertNil(message.interruption, "Relaunch must not turn an in-flight control into a fresh steer")
        XCTAssertEqual(message.phase, .starting)
        var idle = PendingMessage(agentID: "a", input: "x", predecessor: "")
        idle.phase = .queued; XCTAssertNil(idle.interruption)
        var selfTarget = PendingMessage(agentID: "a", input: "x", predecessor: "same", id: "same")
        selfTarget.phase = .queued; XCTAssertNil(selfTarget.interruption)
    }
    func testSteeringUsesQueueOrderAndNeverCancelsItsOwnOrNewerTurn() throws {
        var message = PendingMessage(agentID: "a", input: "Correction", predecessor: "finished", id: "queued")
        message.phase = .queued
        XCTAssertEqual(message.interruption(activeTurns: ["current", "queued"])?.turnID, "current")
        XCTAssertNil(message.interruption(activeTurns: ["queued", "later"]))
        XCTAssertNil(message.interruption(activeTurns: ["later"]))
        XCTAssertNil(message.interruption(activeTurns: []))
        message.predecessor = "current"
        XCTAssertEqual(message.interruption(activeTurns: ["current"])?.turnID, "current", "A stale pre-admission snapshot can still identify the captured predecessor")
        message.phase = .cancelling
        XCTAssertNil(message.interruption(activeTurns: ["current", "queued"]))
    }
    func testLateAdmissionCannotUndoCancellationAndControlSurvivesRestore() throws {
        var message = PendingMessage(agentID: "a", input: "Once", predecessor: "running", id: "queued")
        message.phase = .cancelling
        try message.acknowledge(.object(["turn_id": .string("queued"), "state": .string("accepted"), "accepted_cursor": .string("42")]))
        XCTAssertEqual(message.phase, .cancelling)
        XCTAssertEqual(message.acceptedCursor?.rawValue, "42")
        message.restore()
        XCTAssertEqual(message.phase, .cancelling)
        var intent = PendingTurnCancellation(agentID: "a", turnID: "queued")
        intent.acknowledged = true
        let restored = try JSONDecoder().decode(PendingTurnCancellation.self, from: JSONEncoder().encode(intent))
        XCTAssertEqual(restored, intent)
        XCTAssertEqual(try restored.command.requestSpec().path, "/v1/agents/a/turns/queued/cancel")
        XCTAssertThrowsError(try message.acknowledge(.object(["turn_id": .string("other")])))
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
