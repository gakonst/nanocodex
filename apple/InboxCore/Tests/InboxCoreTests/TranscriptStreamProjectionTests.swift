import XCTest
@testable import InboxCore

final class TranscriptStreamProjectionTests: XCTestCase {
    private func event(_ cursor: Int, _ type: String, _ fields: [String: JSON] = [:], turn: String = "t") throws -> AgentEvent {
        var data = fields
        data["cursor"] = .string(String(cursor)); data["type"] = .string(type); data["turn_id"] = .string(turn)
        return try AgentEvent(.object(data))
    }
    private func delta(_ cursor: Int, _ text: String) throws -> AgentEvent {
        try event(cursor, "event", ["event": .object(["type": .string("assistant.delta"), "payload": .object([
            "text": .string(text), "phase": .string("final_answer"), "item_id": .string("answer")])])])
    }

    func testOverlappingScreensRetainAdmissionAndRankByCompletionCursor() async throws {
        func tool(_ cursor: Int, _ type: String, _ call: String) throws -> AgentEvent {
            try event(cursor, "event", ["event": .object(["type": .string(type), "payload": .object([
                "call_id": .string(call), "tool": .string("computer"),
                "arguments": .object(["action": .string("observe")]),
                "result": .object(["image_url": .string("data:image/png;base64,AQIDBA==")])
            ])])])
        }
        let events = [try tool(10, "tool.call", "a"), try tool(11, "tool.call", "b"),
                      try tool(12, "tool.result", "b"), try tool(13, "tool.result", "a")]
        let projector = TranscriptStreamProjection()
        for end in 1...events.count {
            let prefix = Array(events.prefix(end))
            let rows = try await projector.rows(prefix)
            XCTAssertEqual(rows, transcript(prefix))
        }
        let rows = try await projector.rows(events)
        let first = try XCTUnwrap(rows.first { $0.id.hasSuffix(":a") })
        let second = try XCTUnwrap(rows.first { $0.id.hasSuffix(":b") })
        XCTAssertEqual(first.cursor?.rawValue, "10")
        XCTAssertEqual(second.cursor?.rawValue, "11")
        XCTAssertEqual(first.completionCursor?.rawValue, "13")
        XCTAssertEqual(second.completionCursor?.rawValue, "12")
        XCTAssertTrue(try XCTUnwrap(first.completionCursor) > XCTUnwrap(second.completionCursor))
        XCTAssertTrue(first.tool?.isComputerScreenOutput == true)
    }

    func testIncrementalReplayPreservesInterleavedTurnsAndTerminalFailures() async throws {
        let history = [
            try event(1, "turn_accepted", ["input": .string("first")]),
            try delta(2, "hello"),
            try event(3, "turn_accepted", ["input": .string("next")], turn: "queued"),
            try event(4, "event", ["event": .object(["type": .string("tool.call"), "payload": .object(["tool": .string("exec_command"), "call_id": .string("c"), "arguments": .object(["cmd": .string("date")])])])]),
            try delta(5, " world"),
            try event(6, "turn_failed", ["error": .string("restore failed")]),
            try event(7, "turn_cancelled", turn: "queued")
        ]
        let projector = TranscriptStreamProjection()
        for end in 1...history.count {
            let prefix = Array(history.prefix(end))
            let actual = try await projector.rows(prefix)
            XCTAssertEqual(actual, transcript(prefix))
        }
        let replay = try await projector.rows(history)
        XCTAssertEqual(replay, transcript(history))
        XCTAssertFalse(replay.contains(where: \.running))
        for window in [Array(history.suffix(4)), history, Array(history.prefix(2)), []] {
            let actual = try await projector.rows(window)
            XCTAssertEqual(actual, transcript(window), "Prepend/trim/replacement must rebuild, not merge stale rows")
        }
    }

    func testCancellationDiagnosticAndTerminalShareOneStatusInEitherOrder() async throws {
        func diagnostic(_ cursor: Int, _ message: String = "the turn was cancelled", turn: String = "t", agent: String? = nil, extra: [String: JSON] = [:]) throws -> AgentEvent {
            var payload = extra
            payload["message"] = .string(message)
            var fields: [String: JSON] = ["event": .object(["type": .string("run.error"), "payload": .object(payload)])]
            if let agent { fields["agent_id"] = .string(agent) }
            return try event(cursor, "event", fields, turn: turn)
        }
        for diagnosticFirst in [false, true] {
            let sequence = [
                try delta(1, "Partial answer"),
                try diagnosticFirst ? diagnostic(2) : event(2, "turn_cancelled"),
                try diagnosticFirst ? event(3, "turn_cancelled") : diagnostic(3),
                try diagnostic(4, "Connection reset"),
                try diagnostic(5, turn: "other"),
                try diagnostic(6, agent: "child"),
                try diagnostic(7, extra: ["disposition": .string("retryable")]),
                try diagnostic(8, extra: ["code": .string("storage_failed")]),
                try event(9, "turn_cancelled")
            ]
            let projector = TranscriptStreamProjection()
            var prefix: [AgentEvent] = []
            for item in sequence {
                prefix.append(item)
                let incremental = try await projector.rows(prefix)
                XCTAssertEqual(incremental, transcript(prefix))
            }
            let rows = try await projector.rows(sequence)
            XCTAssertEqual(rows.filter { $0.role == "Status" && $0.text == "Stopped." }.count, 1)
            XCTAssertEqual(rows.filter { $0.text == "Connection reset" }.count, 1)
            XCTAssertEqual(rows.filter { $0.text == "the turn was cancelled" }.count, 4,
                           "Other turns, child agents, retryable diagnostics and explicit failures remain visible")
            XCTAssertFalse(rows.first { $0.role == "Agent" }!.running)
            let status = try XCTUnwrap(rows.first { $0.text == "Stopped." })
            XCTAssertEqual(status.cursor?.rawValue, "2", "Confirmation retains the first status row identity")
            // A page starting at the diagnostic has no terminal proof: preserve it.
            let clipped = [try diagnostic(10)]
            let clippedRows = try await projector.rows(clipped)
            XCTAssertEqual(clippedRows.map(\.text), ["the turn was cancelled"])
        }
    }

    func testStreamingLongHistoryWorkAndOutput() async throws {
        var history = try (1...400).map { index in
            try event(index, "turn_completed", ["final_message": .string(String(repeating: "Earlier answer Ελληνικά. ", count: 64))], turn: "old-\(index)")
        }
        let chunks = try (401...700).map { try delta($0, "streaming ") }
        var baseline: [TranscriptRow] = []
        let fullStart = ContinuousClock.now
        for chunk in chunks { history.append(chunk); baseline = transcript(history) }
        let full = fullStart.duration(to: .now)
        history.removeLast(chunks.count)
        let projector = TranscriptStreamProjection()
        _ = try await projector.rows(history)
        let incrementalStart = ContinuousClock.now
        var projected: [TranscriptRow] = []
        for chunk in chunks { history.append(chunk); projected = try await projector.rows(history) }
        let incremental = incrementalStart.duration(to: .now)
        XCTAssertEqual(projected, baseline)
        print("TRANSCRIPT_STREAM_PERF full=\(full) incremental=\(incremental) retained_turns=400 chunks=300")
    }

    func testIndexedProjectionKeepsTurnStreamAndToolIdentityAcrossRevisions() throws {
        func payload(_ cursor: Int, _ turn: String, _ type: String, _ value: [String: JSON], agent: String? = nil) throws -> AgentEvent {
            var fields: [String: JSON] = ["event": .object(["type": .string(type), "payload": .object(value)])]
            if let agent { fields["agent_id"] = .string(agent) }
            return try event(cursor, "event", fields, turn: turn)
        }
        func text(_ value: String, phase: String = "final_answer", item: String = "answer") -> [String: JSON] {
            ["text": .string(value), "phase": .string(phase), "item_id": .string(item)]
        }
        let events = [
            try event(1, "turn_accepted", ["input": .string("First request")], turn: "first"),
            try payload(2, "first", "assistant.delta", text("Checking ", phase: "commentary", item: "comment")),
            try event(3, "turn_accepted", ["input": .string("Second request")], turn: "second"),
            try payload(4, "first", "assistant.delta", text("Child answer"), agent: "child"),
            try payload(5, "first", "assistant.delta", text("context", phase: "commentary", item: "comment")),
            try payload(6, "first", "assistant.message", text("Revised commentary", phase: "commentary", item: "comment")),
            try payload(7, "first", "assistant.delta", text("Draft ")),
            try payload(8, "second", "assistant.delta", text("Other answer")),
            try payload(9, "first", "assistant.delta", text("answer")),
            try payload(10, "first", "assistant.message", text("Revised answer")),
            try payload(11, "first", "tool.call", ["tool": .string("exec_command"), "call_id": .string("same"), "arguments": .object(["cmd": .string("date")])]),
            try payload(12, "second", "tool.call", ["tool": .string("exec_command"), "call_id": .string("same"), "arguments": .object(["cmd": .string("date")])]),
            try event(13, "turn_completed", ["final_message": .string("Final accepted")], turn: "first"),
            try payload(14, "first", "tool.result", ["tool": .string("exec_command"), "call_id": .string("same"), "result": .object(["output": .string("First tool result"), "exit_code": .number(0)])]),
            try event(15, "turn_cancelled", turn: "second"),
            try payload(16, "third", "assistant.delta", text("Same final", phase: "commentary")),
            try event(17, "turn_completed", ["final_message": .string("Same final")], turn: "third"),
            try event(18, "turn_accepted", ["input": .string("Follow-up")], turn: "third"),
            try event(19, "turn_completed", ["final_message": .string("Fresh final")], turn: "third")
        ]
        var projection = TranscriptProjection()
        for event in events { projection.append([event][...]) }
        let rows = projection.rows
        XCTAssertEqual(rows.filter { $0.turnID == "first" && $0.role == "Agent" && $0.agentID == nil }.map(\.text),
                       ["Revised commentary", "Final accepted"])
        XCTAssertEqual(rows.first { $0.agentID == "child" }?.text, "Child answer")
        XCTAssertEqual(rows.filter { $0.turnID == "third" && $0.role == "Agent" }.map(\.text), ["Same final", "Fresh final"])
        XCTAssertEqual(rows.filter { $0.turnID == "third" && $0.role == "Agent" }.map(\.phase), ["final_answer", "final_answer"])
        let firstTool = try XCTUnwrap(rows.first { $0.turnID == "first" && $0.role == "Tool" }?.tool)
        XCTAssertTrue(firstTool.output.contains { $0.value.contains("First tool result") })
        XCTAssertEqual(rows.first { $0.turnID == "second" && $0.role == "Tool" }?.tool?.status, "Stopped")
        XCTAssertFalse(rows.contains(where: \.running))
        projection.append(events[...])
        XCTAssertEqual(projection.rows, rows, "Replayed frames must not duplicate indexed rows")
    }

    func testWarmTabProjectionReusesHistoryAndCatchesUnprojectedTail() async throws {
        let history = try (1...4000).map { index in
            try event(index, "turn_completed", ["final_message": .string("Answer \(index). " + String(repeating: "Retained tab history. ", count: 16))], turn: "old-\(index)")
        }
        let saved = TranscriptStreamProjection()
        let expected = try await saved.rows(history)
        let coldStart = ContinuousClock.now
        for _ in 0..<3 {
            let rebuilt = try await TranscriptStreamProjection().rows(history)
            XCTAssertEqual(rebuilt, expected)
        }
        let cold = coldStart.duration(to: .now)
        let warmStart = ContinuousClock.now
        for _ in 0..<3 {
            let restored = try await saved.rows(history)
            XCTAssertEqual(restored, expected)
        }
        let warm = warmStart.duration(to: .now)
        // Switching can cancel the pending UI projection after receiving a frame.
        // Reusing the projector must still catch that unprojected suffix once.
        let resumed = history + [try delta(4001, "Arrived before switching.")]
        let projected = try await saved.rows(resumed)
        XCTAssertEqual(projected, transcript(resumed))
        print("TAB_RESTORE_PERF cold=\(cold) warm=\(warm) history_events=4000 restores=3")
    }

    func testInactiveTabBudgetEvictsOversizedAndOldestTabs() {
        XCTAssertEqual(TranscriptRetention.cachedPrefixCount(byteCounts: [16, 16, 16], byteLimit: 24, countLimit: 8), 2)
        XCTAssertEqual(TranscriptRetention.cachedPrefixCount(byteCounts: [30], byteLimit: 24, countLimit: 8), 1)
        XCTAssertEqual(TranscriptRetention.cachedPrefixCount(byteCounts: [1, 1, 1], byteLimit: 24, countLimit: 2), 1)
        XCTAssertEqual(TranscriptRetention.cachedPrefixCount(byteCounts: [], byteLimit: 24, countLimit: 8), 0)
    }
}
