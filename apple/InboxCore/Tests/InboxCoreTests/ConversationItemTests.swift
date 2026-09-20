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

    private func tool(_ id: String, name: String = "exec_command", running: Bool = false, turn: String = "t") -> TranscriptRow {
        var row = TranscriptRow(id: id, role: "Tool", text: name, running: running,
                                tool: ToolPresentation(name: name, arguments: .null))
        row.turnID = turn
        return row
    }

    func testCodeModeOwnsOnlyItsNestedCallsAndKeepsTheirOrder() {
        let batch = tool("t::tool:batch", name: "exec", running: true)
        let first = tool("t::tool:batch/code-1", running: true)
        let second = tool("t::tool:batch/code-2", name: "environment")
        let independent = tool("t::tool:independent")
        let message = TranscriptRow(id: "message", role: "Agent", text: "Working")
        let rows = [batch, first, independent, message, second]
        let items = ConversationItem.group(rows, activeTurns: ["t"])
        XCTAssertEqual(items.map(\.id), [batch.id, independent.id, message.id])
        XCTAssertEqual(items[0].activity.map(\.id), [batch.id, first.id, second.id])
        XCTAssertTrue(items[0].isCodeModeBatch)
        XCTAssertTrue(items[0].isRunning)
        XCTAssertEqual(items.flatMap(\.activity).map(\.id).sorted(), rows.filter { $0.role == "Tool" }.map(\.id).sorted())
        var completed = rows
        completed[0].running = false
        completed[1].running = false
        let finished = ConversationItem.group(completed, activeTurns: ["t"])
        XCTAssertEqual(finished.map(\.id), items.map(\.id))
        XCTAssertFalse(finished[0].isRunning)
    }

    func testOrphansAndOtherScopesStayStandalone() {
        let parent = tool("t::tool:batch", name: "exec")
        let orphan = tool("t::tool:missing/code-1")
        let otherTurn = tool("next::tool:batch/code-1", turn: "next")
        let otherAgent = tool("t:helper:tool:batch/code-1")
        let malformed = tool("t::tool:batch/code-no")
        let rows = [parent, orphan, otherTurn, otherAgent, malformed]
        XCTAssertEqual(ConversationItem.group(rows).map(\.id), rows.map(\.id))
        let child = tool("t::tool:batch/code-1")
        XCTAssertEqual(ConversationItem.group([child]).first?.activity, [child])
        XCTAssertEqual(ConversationItem.group([parent, child]).first?.activity, [parent, child])
    }

    func testParallelBatchesAndQueuedTurnsKeepRunningStateScoped() {
        let a = tool("t::tool:a", name: "exec")
        let b = tool("t::tool:b", name: "exec", running: true)
        let ac = tool("t::tool:a/code-1", running: true)
        let bc = tool("t::tool:b/code-1")
        let groups = ConversationItem.group([a, b, bc, ac], activeTurns: ["t"])
        XCTAssertEqual(groups.map(\.id), [a.id, b.id])
        XCTAssertEqual(groups[0].activity.map(\.id), [a.id, ac.id])
        XCTAssertEqual(groups[1].activity.map(\.id), [b.id, bc.id])
        XCTAssertTrue(groups.allSatisfy(\.isRunning))
        XCTAssertFalse(ConversationItem.group([a, b, bc, ac], activeTurns: ["earlier", "t"]).contains(where: \.isRunning))
    }

    func testRuntimeNestedCallFixtureGroupsDuringStreamingAndReplay() throws {
        func event(_ cursor: Int, _ type: String, call: String, tool: String, value: JSON) throws -> AgentEvent {
            let key = type == "tool.call" ? "arguments" : "result"
            return try AgentEvent(.object([
                "cursor": .string(String(cursor)), "type": .string("event"), "turn_id": .string("t"),
                "event": .object(["type": .string(type), "payload": .object([
                    "call_id": .string(call), "tool": .string(tool), key: value
                ])])
            ]))
        }
        // Mirrors execute_nested_call's parent/code-N IDs, including out-of-order
        // parallel completions. No UI fixture fabricates a separate parent field.
        let calls = [
            try event(1, "tool.call", call: "batch", tool: "exec", value: .string("await Promise.all([tools.exec_command({cmd: 'ls'}), tools.environment({})])")),
            try event(2, "tool.call", call: "batch/code-1", tool: "exec_command", value: .object(["cmd": .string("ls")])),
            try event(3, "tool.call", call: "batch/code-2", tool: "environment", value: .object([:]))
        ]
        let results = [
            try event(4, "tool.result", call: "batch/code-2", tool: "environment", value: .object(["status": .string("ready")])),
            try event(5, "tool.result", call: "batch/code-1", tool: "exec_command", value: .object(["output": .string("file.txt"), "exit_code": .number(0)])),
            try event(6, "tool.result", call: "batch", tool: "exec", value: .string("Completed"))
        ]
        var projection = TranscriptProjection()
        projection.append(calls[...])
        let running = ConversationItem.group(projection.rows, activeTurns: ["t"])
        XCTAssertEqual(running.count, 1)
        XCTAssertEqual(running[0].activity.count, 3)
        XCTAssertTrue(running[0].isRunning)
        projection.append(results[...])
        let finished = ConversationItem.group(projection.rows, activeTurns: ["t"])
        XCTAssertEqual(finished.count, 1)
        XCTAssertEqual(finished[0].id, running[0].id)
        XCTAssertEqual(finished[0].activity.map(\.id), running[0].activity.map(\.id))
        XCTAssertFalse(finished[0].isRunning)
        XCTAssertEqual(finished, ConversationItem.group(transcript(calls + results), activeTurns: ["t"]))
        let encoded = try JSONEncoder().encode(projection.rows)
        XCTAssertEqual(finished, ConversationItem.group(try JSONDecoder().decode([TranscriptRow].self, from: encoded)))
    }

    func testNestedBatchesKeepAllDescendantsVisible() {
        let parent = tool("t::tool:batch", name: "exec")
        let nested = tool("t::tool:batch/code-1", name: "exec")
        let leaf = tool("t::tool:batch/code-1/code-2")
        let items = ConversationItem.group([parent, nested, leaf])
        XCTAssertEqual(items.map(\.id), [parent.id])
        XCTAssertEqual(items[0].activity.map(\.id), [parent.id, nested.id, leaf.id])
    }
}
