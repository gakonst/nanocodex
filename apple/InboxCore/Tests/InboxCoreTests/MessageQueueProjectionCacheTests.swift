import XCTest
@testable import InboxCore

final class MessageQueueProjectionCacheTests: XCTestCase {
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


    private func row(_ id: String, text: String = "Request") -> TranscriptRow {
        var row = TranscriptRow(id: id, role: "You", text: text)
        row.turnID = id
        return row
    }
    private func event(_ cursor: Int, _ type: String, turn: String) throws -> AgentEvent {
        try AgentEvent(.object(["cursor": .string(String(cursor)), "type": .string(type), "turn_id": .string(turn),
            "event": .object(["type": .string("run.started")])]))
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
