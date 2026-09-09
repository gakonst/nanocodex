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

    func testInactiveTabBudgetEvictsOversizedAndOldestTabs() {
        XCTAssertEqual(TranscriptRetention.cachedPrefixCount(byteCounts: [16, 16, 16], byteLimit: 24, countLimit: 8), 2)
        XCTAssertEqual(TranscriptRetention.cachedPrefixCount(byteCounts: [30], byteLimit: 24, countLimit: 8), 1)
        XCTAssertEqual(TranscriptRetention.cachedPrefixCount(byteCounts: [1, 1, 1], byteLimit: 24, countLimit: 2), 1)
        XCTAssertEqual(TranscriptRetention.cachedPrefixCount(byteCounts: [], byteLimit: 24, countLimit: 8), 0)
    }
}
