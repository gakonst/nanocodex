import XCTest
@testable import InboxCore

final class MessageQueuePresentationTests: XCTestCase {
    private func row(_ turn: String, role: String = "You") -> TranscriptRow {
        var row = TranscriptRow(id: "row-" + turn, role: role, text: "Text " + turn)
        row.turnID = turn
        return row
    }
    private func message(_ id: String, phase: PendingMessage.Phase = .queued) -> PendingMessage {
        var message = PendingMessage(agentID: "a", input: "Text " + id, predecessor: "running", id: id)
        message.phase = phase
        return message
    }
    func testRemoteQueueUsesServerOrderBeforeLocalPending() {
        let projection = MessageQueuePresentation(agentID: "a", rows: [row("running"), row("remote"), row("local")],
            pending: [message("local"), message("uploading", phase: .submitting)], activeTurns: ["running", "remote", "local"], executingTurns: ["running"])
        XCTAssertEqual(projection.messages.map(\.id), ["remote", "local", "uploading"])
        XCTAssertEqual(projection.messages.first?.interruption(activeTurns: ["running", "remote", "local"])?.turnID, "running")
        XCTAssertEqual(projection.rows.map(\.turnID), ["running"])
    }
    func testQueueReconstructedWithoutLocalStoragePreservesAttachments() throws {
        var queued = row("remote")
        queued.imageFiles = [try MessageAttachment(name: "Photo.png", mediaType: "image/png", byteCount: 20)]
        let projection = MessageQueuePresentation(agentID: "a", rows: [queued], pending: [], activeTurns: ["outside-history", "remote"], executingTurns: ["outside-history"])
        XCTAssertEqual(projection.attachmentNames["remote"], ["Photo.png"])
        XCTAssertNil(projection.messages.first?.attachments, "Remote attachments are not locally owned files")
        XCTAssertEqual(projection.messages.first?.remoteAdmission, true)
        XCTAssertTrue(projection.rows.isEmpty)
    }
    func testExecutionWinsOverLateSubmissionReceiptAndStaleQueueSnapshot() {
        let projection = MessageQueuePresentation(agentID: "a", rows: [row("started")], pending: [message("started", phase: .submitting)],
            activeTurns: ["older", "started"], executingTurns: ["older", "started"])
        XCTAssertTrue(projection.messages.isEmpty)
        XCTAssertEqual(projection.rows.count, 1)
    }
    func testQueueHeadDoesNotClaimToWaitBehindStalePredecessor() {
        let projection = MessageQueuePresentation(agentID: "a", rows: [row("next")], pending: [message("next")], activeTurns: ["next"])
        XCTAssertEqual(projection.messages.first?.queueTitle, "Sent")
        XCTAssertNil(projection.messages.first?.interruption(activeTurns: ["next"]))
    }
    func testAdoptedRemoteContentRefreshesWithoutLosingControlIntent() {
        var adopted = message("remote", phase: .cancelling)
        adopted.remoteAdmission = true
        adopted.acceptedCursor = Cursor(rawValue: "42")
        adopted.error = "Stop unconfirmed"
        var loaded = row("remote")
        loaded.text = "Actual queued request"
        let projection = MessageQueuePresentation(agentID: "a", rows: [loaded], pending: [adopted],
            activeTurns: ["running", "remote"], executingTurns: ["running"])
        XCTAssertEqual(projection.messages.first?.input, "Actual queued request")
        XCTAssertEqual(projection.messages.first?.phase, .cancelling)
        XCTAssertEqual(projection.messages.first?.error, "Stop unconfirmed")
        XCTAssertEqual(projection.messages.first?.acceptedCursor, adopted.acceptedCursor)
        XCTAssertFalse(adopted.hasFinished(activeTurns: [], stateCursor: Cursor(rawValue: "41")!))
        XCTAssertTrue(adopted.hasFinished(activeTurns: [], stateCursor: Cursor(rawValue: "43")!))
    }
    func testFirstBubbleHasOneStableIdentityBeforeReceiptThroughExecution() throws {
        var pending = PendingMessage(agentID: "a", input: "Show this immediately", predecessor: "", id: "instant")
        let local = MessageQueuePresentation(agentID: "a", rows: [], pending: [pending], activeTurns: [])
        XCTAssertEqual(local.rows.map(\.text), [pending.input])
        var accepted = row("instant")
        accepted.text = pending.input
        accepted.imageFiles = [try MessageAttachment(name: "Photo.png", mediaType: "image/png", byteCount: 20)]
        pending.phase = .queued
        let admitted = MessageQueuePresentation(agentID: "a", rows: [accepted], pending: [pending], activeTurns: [pending.id])
        let started = MessageQueuePresentation(agentID: "a", rows: [accepted], pending: [], activeTurns: [pending.id], executingTurns: [pending.id])
        XCTAssertEqual(local.rows.map(\.id), admitted.rows.map(\.id))
        XCTAssertEqual(admitted.rows, started.rows)
        XCTAssertEqual(admitted.rows.first?.imageFiles, accepted.imageFiles)
    }
    func testMissingRemotePayloadStillOwnsItsQueuePosition() {
        let projection = MessageQueuePresentation(agentID: "a", rows: [row("local")], pending: [message("local")],
            activeTurns: ["running", "remote", "local"], executingTurns: ["running"])
        XCTAssertEqual(projection.messages.map(\.id), ["remote", "local"])
        XCTAssertEqual(projection.messages.first?.remoteAdmission, true)
    }
    func testCancellationDoesNotProduceNormalSubmittedBubble() {
        let projection = MessageQueuePresentation(agentID: "a", rows: [row("cancelled")], pending: [], activeTurns: [], cancelledTurns: ["cancelled"])
        XCTAssertTrue(projection.messages.isEmpty)
        XCTAssertEqual(projection.rows.first?.role, "Status")
        XCTAssertEqual(projection.rows.first?.text, "Cancelled request: Text cancelled")
    }
    func testOtherAgentAndStaleSnapshotDoNotHideOrLoseMessages() {
        let unresolved = message("unconfirmed", phase: .failed)
        let projection = MessageQueuePresentation(agentID: "a", rows: [row("unconfirmed")], pending: [unresolved], activeTurns: [])
        XCTAssertEqual(projection.messages, [unresolved])
        XCTAssertTrue(projection.rows.isEmpty)
        let other = MessageQueuePresentation(agentID: "b", rows: [row("unconfirmed")], pending: [unresolved], activeTurns: [])
        XCTAssertTrue(other.messages.isEmpty)
        XCTAssertEqual(other.rows.count, 1)
    }
}
