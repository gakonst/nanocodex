import XCTest
@testable import InboxCore

final class ToolPresentationTests: XCTestCase {
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
}
