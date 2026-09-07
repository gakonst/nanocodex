import XCTest
@testable import InboxCore

final class ToolPresentationTests: XCTestCase {
    func testRecoveryReplayKeepsOneCommandAndItsOriginalStartTime() throws {
        func event(_ cursor: String, _ time: Double, _ type: String, _ payload: JSON) throws -> AgentEvent {
            try AgentEvent(.object(["cursor": .string(cursor), "created_at": .number(time), "type": .string("event"), "turn_id": .string("t"), "event": .object(["type": .string(type), "payload": payload])]))
        }
        let call: JSON = .object(["call_id": .string("c"), "tool": .string("exec_command"), "arguments": .object(["cmd": .string("sleep 120")])])
        let initial: JSON = .object(["call_id": .string("c"), "tool": .string("exec_command"), "status": .string("completed"), "structured_result": .object(["session_id": .number(42), "output": .string("START\n")])])
        let poll: JSON = .object(["call_id": .string("p"), "tool": .string("write_stdin"), "arguments": .object(["session_id": .number(42)])])
        let rows = try transcript([
            event("1", 1000, "tool.call", call), event("2", 2000, "tool.result", initial),
            event("3", 3000, "tool.call", poll),
            event("4", 57000, "tool.call", call), event("5", 57000, "tool.result", initial),
            event("6", 57000, "tool.call", poll),
            event("7", 58000, "tool.result", .object(["call_id": .string("p"), "tool": .string("write_stdin"), "status": .string("failed"), "structured_result": .string("unknown or stale namespace process session")]))
        ])
        XCTAssertEqual(rows.count, 1)
        XCTAssertEqual(rows[0].tool?.status, "Failed")
        XCTAssertTrue(rows[0].tool!.output.contains { $0.label == "Output" && $0.value == "START\nunknown or stale namespace process session" })
        XCTAssertTrue(rows[0].tool!.output.contains { $0.label == "Elapsed (seconds)" && $0.value == "57" })
        XCTAssertFalse(rows[0].tool!.output.contains { $0.label == "Exit code" })
    }
    func testCommandElapsedTimeIncludesExecutionBetweenPolls() throws {
        func event(_ cursor: String, _ time: Double, _ type: String, _ payload: JSON) throws -> AgentEvent {
            try AgentEvent(.object(["cursor": .string(cursor), "created_at": .number(time), "type": .string("event"), "turn_id": .string("t"), "event": .object(["type": .string(type), "payload": payload])]))
        }
        let events = try [
            event("1", 1788766853390, "tool.call", .object(["call_id": .string("c"), "tool": .string("exec_command"), "arguments": .object(["cmd": .string("cargo test")])])),
            event("2", 1788766854949, "tool.result", .object(["call_id": .string("c"), "tool": .string("exec_command"), "status": .string("completed"), "structured_result": .object(["session_id": .number(42), "output": .string("Compiling\n"), "wall_time_seconds": .number(1.216)])])),
            event("3", 1788766963000, "tool.call", .object(["call_id": .string("poll"), "tool": .string("write_stdin"), "arguments": .object(["session_id": .number(42)])])),
            event("4", 1788766965479, "tool.result", .object(["call_id": .string("poll"), "tool": .string("write_stdin"), "status": .string("completed"), "structured_result": .object(["exit_code": .number(0), "output": .string("13 tests passed"), "wall_time_seconds": .number(1.732)])])),
        ]
        let running = transcript(Array(events.prefix(2)))
        XCTAssertEqual(running[0].tool?.status, "Running")
        XCTAssertFalse(running[0].tool!.output.contains { $0.label == "Elapsed (seconds)" || $0.label == "Wall time seconds" })
        let finished = transcript(events)
        XCTAssertEqual(finished.count, 1)
        XCTAssertEqual(finished[0].tool?.status, "Completed")
        XCTAssertTrue(finished[0].tool!.output.contains { $0.label == "Elapsed (seconds)" && $0.value == "112.089" })
        XCTAssertFalse(finished[0].tool!.output.contains { $0.label == "Wall time seconds" })
    }

    func testYieldedCommandsRetainProgressUntilTheirActualExit() throws {
        func event(_ cursor: String, _ type: String, _ payload: JSON, turn: String = "t") throws -> AgentEvent {
            try AgentEvent(.object(["cursor": .string(cursor), "type": .string("event"), "turn_id": .string(turn), "event": .object(["type": .string(type), "payload": payload])]))
        }
        let start = try [
            event("1", "tool.call", .object(["call_id": .string("c"), "tool": .string("exec_command"), "arguments": .object(["cmd": .string("cargo test --workspace")])])),
            event("2", "tool.result", .object(["call_id": .string("c"), "tool": .string("exec_command"), "status": .string("completed"), "structured_result": .object(["session_id": .number(42), "output": .string("Compiling first\n")])])),
            event("3", "tool.call", .object(["call_id": .string("poll"), "tool": .string("write_stdin"), "arguments": .object(["session_id": .number(42)])]), turn: "next"),
        ]
        let running = transcript(start)
        XCTAssertEqual(running.count, 1)
        XCTAssertTrue(running[0].running)
        XCTAssertEqual(running[0].tool?.status, "Running")
        let pending = try event("4", "tool.result", .object(["call_id": .string("poll"), "tool": .string("write_stdin"), "status": .string("completed"), "structured_result": .object(["session_id": .number(42), "output": .string("Compiling second\n")])]), turn: "next")
        XCTAssertEqual(transcript(start + [pending])[0].tool?.status, "Running")
        for code in [0.0, 101.0] {
            let finished = try event("4", "tool.result", .object(["call_id": .string("poll"), "tool": .string("write_stdin"), "status": .string("completed"), "structured_result": .object(["exit_code": .number(code), "output": .string("Final result\n")])]), turn: "next")
            let rows = transcript(start + [finished])
            XCTAssertEqual(rows.count, 1)
            XCTAssertFalse(rows[0].running)
            XCTAssertEqual(rows[0].tool?.status, code == 0 ? "Completed" : "Failed")
            XCTAssertTrue(rows[0].tool!.output.contains { $0.label == "Output" && $0.value == "Compiling first\nFinal result\n" })
        }
    }

    func testCommandPreservesInputAndFormatsResult() {
        var tool = ToolPresentation(name: "exec_command", arguments: .string("{\"cmd\":\"swift test\",\"workdir\":\"apple\"}"))
        tool.finish(.string("{\"output\":\"14 tests passed\",\"exit_code\":0}"))
        XCTAssertEqual(tool.title, "Run command")
        XCTAssertEqual(tool.status, "Completed")
        XCTAssertTrue(tool.input.contains { $0.label == "Command" && $0.value == "swift test" && $0.code })
        XCTAssertTrue(tool.output.contains { $0.label == "Output" && $0.value == "14 tests passed" })
        XCTAssertFalse(tool.output.contains { $0.value.contains("\"exit_code\"") })
    }
    func testUnknownToolsAndNestedContentStayReadable() {
        var tool = ToolPresentation(name: "mcp__calendar__listUpcomingEvents", arguments: .object(["include_cancelled": .bool(false)]))
        tool.finish(.object(["events": .array([.object(["event_title": .string("Lunch"), "all_day": .bool(true)])])]))
        XCTAssertEqual(tool.title, "List upcoming events")
        XCTAssertEqual(tool.input.first?.value, "No")
        XCTAssertTrue(tool.output.contains { $0.label == "Event title" && $0.value == "Lunch" })
        XCTAssertTrue(tool.output.contains { $0.value == "Yes" })
        XCTAssertEqual(ToolPresentation(name: "user_a1392", arguments: .null, metadata: .object(["tool_name": .string("read_file")])).title, "Read file")
    }
    func testFailuresAndBinaryContentDoNotBecomeSuccessOrGibberish() {
        var tool = ToolPresentation(name: "sandbox_exec", arguments: .null)
        tool.finish(.object(["exit_code": .number(2), "stderr": .string("File not found")]))
        XCTAssertEqual(tool.status, "Failed")
        tool.finish(.object(["isError": .bool(true), "content": .array([.object(["type": .string("text"), "text": .string("Connection lost")])])]))
        XCTAssertEqual(tool.status, "Failed")
        XCTAssertTrue(tool.output.contains { $0.value == "Connection lost" })
        tool.finish(.string("Stopped by user"), state: "cancelled")
        XCTAssertEqual(tool.status, "Stopped")
        tool.finish(.string("Unavailable"), state: "failed")
        XCTAssertEqual(tool.status, "Failed")
        let image = ToolPresentation.fields(.object(["type": .string("image"), "data": .string("base64bytes"), "mimeType": .string("image/png")]), label: "Result")
        XCTAssertEqual(image.first?.value, "Image attachment")
    }
    func testProjectionRetainsOrphanResultsAndInterruptedCalls() throws {
        func event(_ cursor: String, _ type: String, _ payload: JSON) throws -> AgentEvent {
            try AgentEvent(.object(["cursor": .string(cursor), "type": .string("event"), "turn_id": .string("t"), "event": .object(["type": .string(type), "payload": payload])]))
        }
        let call = try event("1", "tool.call", .object(["call_id": .string("c"), "tool": .string("read_file"), "arguments": .object(["path": .string("README.md")])]))
        let result = try event("2", "tool.result", .object(["call_id": .string("c"), "result": .string("Readme contents")]))
        let rows = transcript([call, result, result])
        XCTAssertEqual(rows.count, 1)
        XCTAssertEqual(rows.first?.tool?.subject, "README.md")
        XCTAssertEqual(rows.first?.tool?.output.first?.value, "Readme contents")
        XCTAssertEqual(transcript([result]).count, 1)
        let cancelled = try AgentEvent(.object(["cursor": .string("3"), "type": .string("turn_cancelled"), "turn_id": .string("t")]))
        XCTAssertEqual(transcript([call, cancelled]).first?.tool?.status, "Stopped")
    }

    func testCodeModeProjectionPreservesBothResultFormsAndHidesEmbeddedBytes() throws {
        let image: JSON = .object(["type": .string("input_image"), "image_url": .string("data:image/png;base64,PRIVATE_IMAGE_BYTES")])
        let raw: JSON = .array([.object(["type": .string("input_text"), "text": .string("## Chart ready")]), image])
        let structured: JSON = .object(["exit_code": .number(0), "content": .array([.object(["type": .string("resource"), "resource": .object([
            "uri": .string("artifact:///chart.csv"), "mimeType": .string("text/csv"), "blob": .string("PRIVATE_FILE_BYTES")
        ])])])])
        let result = try AgentEvent(.object(["cursor": .string("1"), "type": .string("event"), "turn_id": .string("t"), "event": .object([
            "type": .string("tool.result"), "payload": .object(["call_id": .string("exec"), "tool": .string("functions.exec"),
                "structured_result": structured, "result": .string(raw.pretty)])
        ])]))
        let rows = transcript([result, result])
        let tool = try XCTUnwrap(rows.first?.tool)
        XCTAssertEqual(rows.count, 1)
        XCTAssertEqual(tool.generatedResults?.count, 2)
        XCTAssertEqual(tool.generatedIncludesText, true)
        XCTAssertTrue(tool.generatedResults?.contains { $0.contains("PRIVATE_IMAGE_BYTES") } == true)
        XCTAssertTrue(tool.generatedResults?.contains { $0.contains("PRIVATE_FILE_BYTES") } == true)
        XCTAssertFalse(tool.output.contains { $0.value.contains("PRIVATE_FILE_BYTES") || $0.value.contains("PRIVATE_IMAGE_BYTES") })
        let restored = try JSONDecoder().decode(TranscriptRow.self, from: JSONEncoder().encode(rows[0]))
        XCTAssertEqual(restored.tool?.generatedResults, tool.generatedResults)
        XCTAssertFalse(ToolPresentation.fields(raw, label: "Result").contains { $0.value.contains("PRIVATE_IMAGE_BYTES") })
        let markdown: JSON = .object(["type": .string("input_text"), "text": .string("Chart: ![Preview](data:image/png;base64,PRIVATE_MARKDOWN_BYTES)")])
        XCTAssertFalse(ToolPresentation.fields(markdown, label: "Result").contains { $0.value.contains("PRIVATE_MARKDOWN_BYTES") })
    }
}
