import XCTest
@testable import InboxCore

final class MessageQueueProjectionCacheTests: XCTestCase {
    func testAcceptedDirectSteeringRetainsOrdinaryMessageWithoutQueueOrSuccessNoise() throws {
        var transfer = SteeringTransfer(agentID: "agent", sourceTurnID: "local", targetTurnID: "running", direct: true, sourceInput: "Use the small example")
        transfer.wasAccepted = true
        transfer.phase = .accepted
        transfer = try JSONDecoder().decode(SteeringTransfer.self, from: JSONEncoder().encode(transfer))
        var cache = MessageQueueProjectionCache()
        let value = cache.presentation(agentID: "agent", events: [], rows: [], pending: [], steeringTransfers: [transfer], activeTurns: ["running"], isDemo: false)
        XCTAssertTrue(value.messages.allSatisfy { $0.id != "local" })
        XCTAssertEqual(value.rows.map(\.role), ["You"])
        XCTAssertEqual(value.rows.map(\.text), ["Use the small example"])
        XCTAssertEqual(value.rows.first?.detail, "")
        cache.invalidate()
        let otherWindow = cache.presentation(agentID: "agent", events: [], rows: [], pending: [], steeringTransfers: [transfer], activeTurns: [], isDemo: false)
        XCTAssertTrue(otherWindow.rows.isEmpty)
    }

    func testPendingDirectSteeringShowsUnconfirmedOrRejectedErrorWithoutDuplicateMessage() {
        for phase: SteeringTransfer.Phase in [.unconfirmed, .ready] {
            var transfer = SteeringTransfer(agentID: "agent", sourceTurnID: "local", targetTurnID: "running", direct: true, sourceInput: "Keep my correction")
            transfer.phase = phase
            transfer.error = "Delivery could not be confirmed"
            var pending = PendingMessage(agentID: "agent", input: "Keep my correction", predecessor: "", id: "local")
            pending.phase = .starting
            var cache = MessageQueueProjectionCache()
            let result = cache.presentation(agentID: "agent", events: [], rows: [], pending: [pending], steeringTransfers: [transfer], activeTurns: ["running"], isDemo: false)
            let messages = result.rows.filter { $0.turnID == "local" }
            XCTAssertEqual(messages.count, 1)
            XCTAssertEqual(messages.first?.role, "You")
            XCTAssertEqual(messages.first?.detail, transfer.error)
            XCTAssertEqual(result.messages.filter { $0.id == "local" }.count, 1)
        }
    }

    func testConsumedDirectSteeringKeepsMessageAndDisplaysWithdrawalError() {
        var transfer = SteeringTransfer(agentID: "agent", sourceTurnID: "local", targetTurnID: "running", direct: true, sourceInput: "Keep the small example")
        transfer.wasAccepted = true
        transfer.phase = .withdrawing
        var cache = MessageQueueProjectionCache()
        let withdrawing = cache.presentation(agentID: "agent", events: [], rows: [], pending: [], steeringTransfers: [transfer], activeTurns: ["running"], isDemo: false)
        XCTAssertEqual(withdrawing.rows.first?.detail, "Withdrawing steering…")
        transfer.phase = .accepted
        transfer.error = "Steering could not be withdrawn; it may already be in use."
        cache.invalidate()
        let consumed = cache.presentation(agentID: "agent", events: [], rows: [], pending: [], steeringTransfers: [transfer], activeTurns: ["running"], isDemo: false)
        XCTAssertEqual(consumed.rows.first?.text, "Keep the small example")
        XCTAssertEqual(consumed.rows.first?.role, "You")
        XCTAssertEqual(consumed.rows.first?.detail, transfer.error)
        XCTAssertFalse(consumed.rows.contains { $0.text.contains("Steering withdrawn") })
    }

    func testDirectSteeringRetainsWithdrawnAndUnconfirmedStatusAfterPendingRetires() {
        for phase: SteeringTransfer.Phase in [.withdrawn, .unconfirmed] {
            var transfer = SteeringTransfer(agentID: "agent", sourceTurnID: "local", targetTurnID: "running", direct: true, sourceInput: "Keep this request")
            transfer.phase = phase
            transfer.error = phase == .unconfirmed ? "Receipt unavailable" : nil
            var cache = MessageQueueProjectionCache()
            let value = cache.presentation(agentID: "agent", events: [], rows: [], pending: [], steeringTransfers: [transfer], activeTurns: ["running"], isDemo: false)
            XCTAssertEqual(value.rows.first?.role, "Status")
            XCTAssertEqual(value.rows.first?.text, (phase == .withdrawn ? "Steering withdrawn: " : "Steering delivery unconfirmed: ") + "Keep this request")
            XCTAssertEqual(value.rows.first?.detail, transfer.error ?? "")
        }
    }

    func testDirectUploadedAttachmentsSurviveAcceptanceAndRestore() throws {
        let image = try MessageAttachment(name: "synthetic.png", mediaType: "image/png", byteCount: 123)
        let video = try MessageAttachment(name: "synthetic.mp4", mediaType: "video/mp4", byteCount: 456,
            video: VideoAttachmentInfo(duration: 3, timestamps: [], promptByteCount: 100, original: true, hasAudio: true))
        var transfer = SteeringTransfer(agentID: "agent", sourceTurnID: "local", targetTurnID: "running", direct: true, sourceInput: "Inspect both")
        transfer.sourcePayload = .array([.object(["type": .string("text"), "text": .string("Inspect both")])]
            + (try image.originalContent(path: image.originalPath)) + (try video.originalContent(path: video.originalPath)))
        transfer.wasAccepted = true; transfer.phase = .accepted
        let restored = try JSONDecoder().decode(SteeringTransfer.self, from: JSONEncoder().encode(transfer))
        var cache = MessageQueueProjectionCache()
        let value = cache.presentation(agentID: "agent", events: [], rows: [], pending: [], steeringTransfers: [restored], activeTurns: ["running"], isDemo: false)
        let row = try XCTUnwrap(value.rows.first)
        XCTAssertEqual(row.text, "Inspect both")
        XCTAssertEqual(row.imageFiles, [image])
        XCTAssertEqual(row.videos?.first?.path, video.originalPath)
        XCTAssertEqual(row.videos?.first?.hasAudio, true)
    }

    func testDirectCorrectionsStayBeforeFutureOutputAndKeepTapOrderAndIdentity() throws {
        func output(_ id: String, _ cursor: Int, turn: String = "running") -> TranscriptRow {
            var value = TranscriptRow(id: id, role: "Assistant", text: id)
            value.turnID = turn; value.cursor = Cursor(rawValue: String(cursor))
            return value
        }
        var transfers = ["first", "second"].map { id in
            var value = SteeringTransfer(agentID: "agent", sourceTurnID: id, targetTurnID: "running",
                direct: true, sourceInput: id, sourceCursor: Cursor(rawValue: "10"), sourceRowID: "before")
            value.wasAccepted = true; value.phase = .accepted
            return value
        }
        transfers = try JSONDecoder().decode([SteeringTransfer].self, from: JSONEncoder().encode(transfers))
        var cache = MessageQueueProjectionCache()
        func project(_ rows: [TranscriptRow], events: [AgentEvent] = [], active: [String] = ["running"]) -> [TranscriptRow] {
            cache.invalidateHistory()
            return cache.presentation(agentID: "agent", events: events, rows: rows, pending: [],
                steeringTransfers: transfers, activeTurns: active, isDemo: false).rows
        }
        let initial = project([output("before", 10)])
        XCTAssertEqual(initial.map(\.id), ["before", "first:user", "second:user"])
        let later = project([output("before", 10), output("tool-result", 11), output("future-turn", 20, turn: "future")])
        XCTAssertEqual(later.map(\.id), ["before", "first:user", "second:user", "tool-result", "future-turn"])
        XCTAssertEqual(later.filter { $0.role == "You" }.map(\.id), initial.filter { $0.role == "You" }.map(\.id))
        // The anchor row can disappear while non-row events still span its cursor.
        let paged = try project([output("later", 12)], events: [event(9, "event", turn: "running"), event(12, "event", turn: "running")])
        XCTAssertEqual(paged.map(\.id), ["first:user", "second:user", "later"])
        XCTAssertEqual(project([output("older-window", 5)]).map(\.id), ["older-window"])
        XCTAssertEqual(project([output("newer-window", 12)]).map(\.id), ["newer-window"])
        XCTAssertEqual(project([output("other-turn", 10, turn: "other")], active: []).map(\.id), ["other-turn"])
        var durable = TranscriptRow(id: "first:user", role: "You", text: "first")
        durable.turnID = "running"; durable.cursor = Cursor(rawValue: "10")
        XCTAssertEqual(try project([output("before", 10), durable, output("later", 12)], events: [event(10, "event", turn: "running"), event(12, "event", turn: "running")]).filter { $0.role == "You" && $0.text == "first" }.count, 1)
    }

    func testLegacySteeringDecodesWithoutPositionOrPayload() throws {
        let data = Data(#"{"agentID":"agent","sourceTurnID":"local","targetTurnID":"running","phase":"accepted","direct":true,"sourceInput":"Correction","withdrawRequested":false,"wasAccepted":true}"#.utf8)
        let transfer = try JSONDecoder().decode(SteeringTransfer.self, from: data)
        XCTAssertNil(transfer.sourceCursor)
        XCTAssertNil(transfer.sourceRowID)
        XCTAssertNil(transfer.sourcePayload)
    }

    private func row(_ id: String, text: String = "Request") -> TranscriptRow {
        var row = TranscriptRow(id: id, role: "You", text: text)
        row.turnID = id
        return row
    }
    private func event(_ cursor: Int, _ type: String, turn: String) throws -> AgentEvent {
        try AgentEvent(.object(["cursor": .string(String(cursor)), "type": .string(type), "turn_id": .string(turn),
            "event": .object(["type": .string("run.started")])]))
    }

    func testRepeatedComposerReadsDoNoProjectionOrHistoryWork() throws {
        var cache = MessageQueueProjectionCache()
        let events = try (1...10_000).map { try event($0, "event", turn: "running") }
        let rows = (1...1_000).map { row("row-\($0)") }
        let revision = cache.revision
        for _ in 0..<100 {
            let result = cache.presentation(agentID: "a", events: events, rows: rows, pending: [],
                steeringTransfers: [], activeTurns: ["running", "queued"], isDemo: false)
            XCTAssertEqual(result.messages.map(\.id), ["queued"])
        }
        XCTAssertEqual(cache.revision, revision)
        XCTAssertEqual(cache.projectionCount, 1)
        XCTAssertEqual(cache.historyScanCount, 1)
    }

    func testRowsPendingActiveAndDemoInvalidationReuseHistorySummary() {
        var cache = MessageQueueProjectionCache()
        var rows = [row("head")]
        var pending: [PendingMessage] = []
        var active = ["head"]
        var demo = false
        func read() -> MessageQueuePresentation {
            cache.presentation(agentID: "a", events: [], rows: rows, pending: pending,
                steeringTransfers: [], activeTurns: active, isDemo: demo)
        }
        XCTAssertEqual(read().messages.map(\.id), ["head"])
        let initial = cache.revision
        rows[0].text = "Edited request"; cache.invalidate()
        XCTAssertNotEqual(cache.revision, initial)
        XCTAssertEqual(read().messages.first?.input, "Edited request")
        var local = PendingMessage(agentID: "a", input: "Local send", predecessor: "head", id: "local")
        local.phase = .cancelling
        pending = [local]; cache.invalidate()
        XCTAssertEqual(read().messages.last?.phase, .cancelling)
        active = ["local"]; cache.invalidate()
        XCTAssertEqual(read().messages.first?.id, "local")
        demo = true; cache.invalidate()
        XCTAssertTrue(read().messages.isEmpty, "The demo head is executing")
        XCTAssertEqual(cache.projectionCount, 5)
        XCTAssertEqual(cache.historyScanCount, 1)
    }

    func testEventAppendPrependTrimAndResetReplaceDerivedSets() throws {
        var cache = MessageQueueProjectionCache()
        let started = try event(2, "event", turn: "head")
        let cancelled = try event(1, "turn_cancelled", turn: "queued")
        func read(_ events: [AgentEvent]) -> MessageQueuePresentation {
            cache.invalidateHistory()
            return cache.presentation(agentID: "a", events: events, rows: [row("queued")], pending: [],
                steeringTransfers: [], activeTurns: ["head", "queued"], isDemo: false)
        }
        XCTAssertEqual(read([]).messages.map(\.id), ["head", "queued"])
        XCTAssertEqual(read([started]).messages.map(\.id), ["queued"])
        let older = read([cancelled, started])
        XCTAssertTrue(older.messages.isEmpty)
        XCTAssertEqual(older.rows.first?.text, "Cancelled request: Request")
        XCTAssertEqual(read([started]).messages.map(\.id), ["queued"], "Trim drops old cancellation state")
        XCTAssertEqual(read([]).messages.map(\.id), ["head", "queued"], "Reset drops execution state")
        XCTAssertEqual(cache.historyScanCount, 5)
    }

    func testSteeringAndPendingChangesRefreshDisplayedSourceWithoutHistoryRescan() throws {
        var cache = MessageQueueProjectionCache()
        let events = [try event(1, "turn_cancelled", turn: "source")]
        var transfer = SteeringTransfer(agentID: "a", sourceTurnID: "source", targetTurnID: "head")
        var pending = [PendingMessage(agentID: "a", input: "Request", predecessor: "head", id: "source")]
        func read() -> MessageQueuePresentation {
            cache.presentation(agentID: "a", events: events, rows: [row("source")], pending: pending,
                steeringTransfers: [transfer], activeTurns: ["head", "source"], isDemo: false)
        }
        XCTAssertTrue(read().rows.isEmpty)
        transfer.phase = .unconfirmed; transfer.error = "No receipt"; cache.invalidate()
        XCTAssertTrue(read().rows.isEmpty)
        pending = []; cache.invalidate()
        XCTAssertEqual(read().rows.first?.text, "Steering delivery unconfirmed: Request")
        transfer.wasAccepted = true; transfer.phase = .accepted; cache.invalidate()
        XCTAssertEqual(read().rows.first?.role, "You")
        XCTAssertEqual(read().rows.first?.detail, "No receipt")
        transfer.phase = .withdrawn; cache.invalidate()
        XCTAssertEqual(read().rows.first?.text, "Steering withdrawn: Request")
        XCTAssertEqual(cache.historyScanCount, 1)
    }

    func testFocusChangeCannotReturnOtherAgentQueue() {
        var cache = MessageQueueProjectionCache()
        let local = PendingMessage(agentID: "a", input: "Only A", predecessor: "", id: "local")
        let first = cache.presentation(agentID: "a", events: [], rows: [], pending: [local],
            steeringTransfers: [], activeTurns: [], isDemo: false)
        XCTAssertEqual(first.rows.first?.text, "Only A")
        cache.invalidateHistory()
        let second = cache.presentation(agentID: "b", events: [], rows: [], pending: [local],
            steeringTransfers: [], activeTurns: [], isDemo: false)
        XCTAssertTrue(second.messages.isEmpty)
        XCTAssertTrue(second.rows.isEmpty)
    }
}
