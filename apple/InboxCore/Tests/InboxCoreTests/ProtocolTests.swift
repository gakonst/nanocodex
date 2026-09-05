import XCTest
@testable import InboxCore

final class ProtocolTests: XCTestCase {
    func event(_ cursor: String, _ type: String, _ fields: [String: JSON] = [:]) throws -> AgentEvent {
        try AgentEvent(.object(fields.merging(["cursor": .string(cursor), "type": .string(type), "turn_id": .string("t")]) { a, _ in a }))
    }
    func state(_ cursor: String, turns: [String]) -> JSON {
        .object(["agent_id": .string("agent"), "latest_event_cursor": .string(cursor), "active_turns": .array(turns.map(JSON.string))])
    }
    func testDecimalCursorsRemainExact() {
        XCTAssertLessThan(Cursor(rawValue: "9007199254740992")!, Cursor(rawValue: "9007199254740993")!)
        XCTAssertLessThan(Cursor(rawValue: "99")!, Cursor(rawValue: "100")!)
        for value in ["", "01", "-1", "1.2", "١"] { XCTAssertNil(Cursor(rawValue: value)) }
    }
    func testSSEMultilineAndCursorHeartbeats() throws {
        var parser = SSEParser()
        XCTAssertNil(try parser.append(line: ": cursor 9007199254740993"))
        XCTAssertEqual(try parser.append(line: "")?.cursor?.rawValue, "9007199254740993")
        for line in ["id: 14", "event: message", "data: {\"type\":\"turn_completed\",", "data: \"turn_id\":\"t\",\"final_message\":\"Done\"}"] { XCTAssertNil(try parser.append(line: line)) }
        let frame = try XCTUnwrap(parser.append(line: ""))
        XCTAssertEqual(frame.event?.cursor.rawValue, "14")
        XCTAssertEqual(frame.event?.data["final_message"].string, "Done")
        XCTAssertNil(try parser.append(line: ""))
    }
    func testSSEByteBoundariesPreserveUnicodeAndBlankLines() throws {
        let payload = "id: 7\r\ndata: {\"type\":\"turn_completed\",\"final_message\":\"Έτοιμο 👋\"}\r\n\r\n"
        var parser = SSEParser(), frames: [SSEFrame] = []
        for byte in payload.utf8 { if let frame = try parser.append(byte: byte) { frames.append(frame) } }
        XCTAssertEqual(frames.count, 1)
        XCTAssertEqual(frames[0].event?.data["final_message"].string, "Έτοιμο 👋")
    }
    func testStaleHistoryCannotReplaceLatestPreview() throws {
        var card = AgentCard(id: "agent", title: "Test")
        card.apply(events: [try event("20", "turn_completed", ["final_message": .string("New")])])
        card.apply(events: [try event("10", "turn_completed", ["final_message": .string("Old")])])
        XCTAssertEqual(card.preview, "New")
    }
    func testMalformedHistoryRejected() throws {
        let e1 = try event("12", "turn_accepted").data, e2 = try event("11", "turn_completed").data
        XCTAssertThrowsError(try EventPage(.object(["data": .array([e1, e2]), "has_more": .bool(false), "latest_cursor": .string("12")])))
    }
    func testReplayCannotResurrectCompletedTurn() throws {
        var card = AgentCard(id: "agent", title: "Test")
        try card.apply(state: state("20", turns: []))
        card.apply(events: [try event("10", "turn_accepted")])
        XCTAssertFalse(card.isRunning)
        card.apply(events: [try event("21", "turn_accepted")])
        XCTAssertTrue(card.isRunning)
        try card.apply(state: state("20", turns: []))
        XCTAssertTrue(card.isRunning, "An older poll must not clobber a live stream update")
        card.apply(events: [try event("22", "turn_completed", ["final_message": .string("Done")])])
        XCTAssertFalse(card.isRunning)
        XCTAssertTrue(card.needsAttention(seen: Cursor(rawValue: "20")))
        XCTAssertFalse(card.needsAttention(seen: Cursor(rawValue: "22")))
    }
    func testTranscriptDeduplicatesReplayAndSeparatesSubagents() throws {
        func delta(_ cursor: String, _ agent: String, _ text: String) throws -> AgentEvent {
            try event(cursor, "event", ["agent_id": .string(agent), "event": .object(["type": .string("assistant.delta"), "payload": .object(["text": .string(text)])])])
        }
        let first = try delta("1", "a", "One")
        let values = [first, first, try delta("2", "b", "Two"), try delta("3", "b", " more")]
        XCTAssertEqual(transcript(values).map(\.text), ["One", "Two more"])
    }
    func testDeckDoesNotSwitchAgentsOnRefresh() {
        var deck = InboxDeck()
        deck.reconcile(["a", "b"]); deck.focus("b")
        deck.reconcile(["c", "a", "b"])
        XCTAssertEqual(deck.focusedID, "b")
        deck.advance(); XCTAssertEqual(deck.focusedID, "a")
        deck.back(); XCTAssertEqual(deck.focusedID, "b")
        deck.reconcile(["c"]); XCTAssertEqual(deck.focusedID, "c")
        deck.reconcile(["a", "b", "c"]); deck.focus("b")
        deck.prioritize(["c", "b", "a"])
        XCTAssertEqual(deck.focusedID, "b")
        deck.advance(); XCTAssertEqual(deck.focusedID, "c")
    }
    func testCommandCapturesTargetAndFollowUpIdentity() throws {
        let command = AgentCommand(agentID: "agent-a", turnID: "turn:1", input: "Focus on reconnect", kind: .steer)
        let spec = try command.requestSpec()
        XCTAssertEqual(spec.path, "/v1/agents/agent-a/turns/turn%3A1/steer")
        XCTAssertEqual(spec.body?["input"].string, "Focus on reconnect")
        let follow = AgentCommand(agentID: "agent-b", input: "Continue", kind: .followUp, requestID: "stable-id")
        XCTAssertEqual(try follow.requestSpec().key, "inbox:stable-id")
        XCTAssertEqual(try follow.requestSpec().body?["id"].string, "stable-id")
        XCTAssertThrowsError(try AgentCommand(agentID: "../other", kind: .stop).requestSpec())
    }
    func testCredentialBoundaries() throws {
        let key = "ncx_live_abcdefghijkl_" + String(repeating: "a", count: 43)
        for origin in ["http://example.com", "https://example.com/path", "https://example.com?secret=x", "https://user@example.com"] {
            XCTAssertThrowsError(try AccountCredential(origin: origin, apiKey: key))
        }
        let client = ManagedClient(credential: try AccountCredential(origin: "https://example.com/", apiKey: key))
        defer { client.close() }
        let request = try client.request(path: "/v1/agents")
        XCTAssertEqual(request.url?.absoluteString, "https://example.com/v1/agents")
        XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer " + key)
        XCTAssertNil(request.url?.query)
        XCTAssertThrowsError(try client.request(path: "//example.org/v1/agents"))
    }
}
