import XCTest
@testable import InboxCore

final class ProtocolTests: XCTestCase {
    func testActivityCoalescesPhasesAndSubagentsWithoutHidingAnswers() throws {
        func output(_ cursor: String, _ type: String, _ text: String, phase: String? = nil, agent: String? = nil) throws -> AgentEvent {
            var payload: [String: JSON] = ["text": .string(text)]
            if let phase { payload["phase"] = .string(phase); payload["item_id"] = .string(phase) }
            var fields: [String: JSON] = ["event": .object(["type": .string(type), "payload": .object(payload)])]
            if let agent { fields["agent_id"] = .string(agent) }
            return try event(cursor, "event", fields)
        }
        var events = [try event("1", "turn_accepted", ["input": .string("Find the answer")]),
                      try output("2", "reasoning.summary.delta", "Compare sources."),
                      try output("3", "assistant.delta", "Checking sources.", phase: "commentary"),
                      try output("4", "assistant.delta", "A helper update.", agent: "helper")]
        let live = ConversationItem.group(transcript(events), activeTurns: ["t"])
        XCTAssertEqual(live.count, 2)
        let groupID = try XCTUnwrap(live.last?.id)
        XCTAssertEqual(live.last?.activity.count, 3)
        XCTAssertEqual(live.last?.isRunning, true)
        // A queued acceptance must not split the first response or absorb its work.
        events.append(try event("5", "turn_accepted", ["turn_id": .string("next:turn"), "input": .string("Follow up")]))
        events.append(try output("6", "assistant.delta", "The answer", phase: "final_answer"))
        events.append(try output("7", "assistant.message", "The answer", phase: "final_answer"))
        events.append(try event("8", "turn_completed", ["final_message": .string("The answer")]))
        let rows = transcript(events)
        let grouped = ConversationItem.group(rows)
        XCTAssertEqual(grouped.map(\.id).filter { $0 == groupID }.count, 1)
        XCTAssertEqual(grouped.compactMap(\.message).map(\.text), ["Find the answer", "The answer", "Follow up"])
        XCTAssertFalse(grouped.contains(where: \.isRunning))
        XCTAssertEqual(rows.filter { $0.phase == "commentary" }.map(\.text), ["Checking sources."])
        XCTAssertEqual(try JSONDecoder().decode([TranscriptRow].self, from: JSONEncoder().encode(rows)), rows)
        XCTAssertEqual(ConversationItem.group([.init(id: "legacy", role: "Agent", text: "An untagged answer")]).first?.message?.text, "An untagged answer")
    }

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
        XCTAssertNil(try parser.append(line: ": keepalive"))
        let heartbeat = try XCTUnwrap(parser.append(line: ""))
        XCTAssertNil(heartbeat.event)
        XCTAssertNil(heartbeat.cursor)
    }
    func testSSEByteBoundariesPreserveUnicodeAndBlankLines() throws {
        let payload = "id: 7\r\ndata: {\"type\":\"turn_completed\",\"final_message\":\"Έτοιμο 👋\"}\r\n\r\n"
        var parser = SSEParser(), frames: [SSEFrame] = []
        for byte in payload.utf8 { if let frame = try parser.append(byte: byte) { frames.append(frame) } }
        XCTAssertEqual(frames.count, 1)
        XCTAssertEqual(frames[0].event?.data["final_message"].string, "Έτοιμο 👋")
        XCTAssertEqual(frames[0].payloadBytes, #"{"type":"turn_completed","final_message":"Έτοιμο 👋"}"#.utf8.count)
    }
    func testRecentConversationOrderUsesDurableActivityAndIgnoresStaleReplay() throws {
        var old = AgentCard(id: "old", title: "Older conversation", updatedAt: 1000)
        var recent = AgentCard(id: "recent", title: "Recent conversation", updatedAt: 2000)
        recent.activeTurns = ["running"]
        func order() -> [String] { [old, recent].sorted(by: AgentCard.mostRecentFirst).map(\.id) }
        XCTAssertEqual(order(), ["recent", "old"])
        old.apply(events: [try event("20", "turn_accepted", ["created_at": .number(3000), "input": .string("Use this again")])])
        XCTAssertEqual(order(), ["old", "recent"])
        recent.apply(events: [try event("9", "turn_completed", ["created_at": .number(4000), "final_message": .string("New reply")])])
        XCTAssertEqual(order(), ["recent", "old"], "Activity timestamps, not per-agent cursors or running status, determine recency")
        recent.apply(events: [try event("1", "turn_accepted", ["created_at": .number(500)])])
        recent.apply(events: [try event("10", "turn_completed")])
        recent.apply(events: [try event("11", "turn_completed", ["created_at": .number(-1)])])
        XCTAssertEqual(recent.updatedAt, 4000)
        XCTAssertEqual(order(), ["recent", "old"])
        old.updatedAt = 4000
        XCTAssertEqual(order(), ["old", "recent"], "Equal timestamps have a deterministic order")
    }

    func testStaleHistoryCannotReplaceLatestPreview() throws {
        var card = AgentCard(id: "agent", title: "Test")
        card.apply(events: [try event("20", "turn_completed", ["final_message": .string("New")])])
        card.apply(events: [try event("10", "turn_completed", ["final_message": .string("Old")])])
        XCTAssertEqual(card.preview, "New")
    }
    func testCardRetainsLatestExchangeBeforeFocusedHistoryLoads() throws {
        let image = "data:image/png;base64,aGVsbG8="
        let answer = "# Full reply\n\n" + String(repeating: "Keep the entire Markdown block. ", count: 80)
        let events = [
            try event("1", "turn_accepted", ["input": .string("Earlier question")]),
            try event("2", "turn_completed", ["final_message": .string("Earlier answer")]),
            try event("3", "turn_accepted", ["input": .array([
                .object(["type": .string("text"), "text": .string("Describe this")]),
                .object(["type": .string("image"), "image_url": .string(image)])
            ])]),
            try event("4", "turn_completed", ["final_message": .string(answer)])
        ]
        var card = AgentCard(id: "agent", title: "Test")
        card.apply(events: events)
        XCTAssertEqual(card.previewRows.map(\.text), ["Describe this", answer])
        XCTAssertEqual(card.previewRows.first?.images, [image])
        card.apply(events: Array(events.prefix(2)))
        XCTAssertEqual(card.previewRows.map(\.text), ["Describe this", answer], "A stale poll cannot replace either side of the cached exchange")
        card.apply(events: events, transcriptRows: transcript(events))
        XCTAssertEqual(card.previewRows.map(\.text), ["Describe this", answer], "Focused and background history must produce the same card")
        card.apply(events: events + [try event("5", "turn_accepted", ["input": .string("Next question")])])
        XCTAssertEqual(card.previewRows.map(\.text), ["Next question"], "Do not pair a new user message with the previous answer")
    }
    func testMalformedHistoryRejected() throws {
        let e1 = try event("12", "turn_accepted").data, e2 = try event("11", "turn_completed").data
        XCTAssertThrowsError(try EventPage(.object(["data": .array([e1, e2]), "has_more": .bool(false), "latest_cursor": .string("12")])))
    }
    func testCardPreviewNeverFallsBackToInternalActivity() throws {
        let position = try event("10", "turn_completed")
        var commentary = TranscriptRow(id: "commentary", role: "Agent", text: "INTERNAL_PROGRESS")
        commentary.phase = "commentary"
        var delegated = TranscriptRow(id: "delegated", role: "Agent", text: "INTERNAL_SUBAGENT_RESULT")
        delegated.agentID = "child"
        let activity = [commentary, delegated,
                        TranscriptRow(id: "thinking", role: "Thinking", text: "INTERNAL_REASONING"),
                        TranscriptRow(id: "tool", role: "Tool", text: "Memory")]
        var card = AgentCard(id: "agent", title: "Test")
        let answer = TranscriptRow(id: "answer", role: "Agent", text: "{\"requested_json\":true}")
        card.apply(events: [position], transcriptRows: [answer] + activity)
        XCTAssertEqual(card.preview, answer.text, "User-facing answers may legitimately contain JSON")
        card.apply(events: [position], transcriptRows: activity)
        XCTAssertTrue(card.preview.isEmpty, "Partial history with only tool or internal rows has no reply preview")
        XCTAssertTrue(card.previewRows.isEmpty)
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
    func testCompletedStatusSurvivesLaterInternalEventsAfterRelaunch() throws {
        var card = AgentCard(id: "agent", title: "Test")
        try card.apply(state: state("30", turns: []))
        card.apply(events: [try event("10", "turn_accepted"),
                            try event("20", "turn_completed", ["final_message": .string("Done")]),
                            try event("30", "event", ["event": .object(["type": .string("managed.voice.context")])])])
        XCTAssertEqual(card.status, "Ready", "A later internal event must not hide the finished task on restore")
        XCTAssertTrue(card.needsAttention(seen: Cursor(rawValue: "19")))
        XCTAssertFalse(card.needsAttention(seen: Cursor(rawValue: "20")), "Internal activity cannot make an already reviewed reply unread")
        card.apply(events: [try event("15", "turn_failed")])
        XCTAssertEqual(card.status, "Ready", "Older history cannot replace the latest outcome")
    }
    func testNewTurnDoesNotInheritAnOlderCompletedStatus() throws {
        var card = AgentCard(id: "agent", title: "Test")
        try card.apply(state: state("20", turns: []))
        card.apply(events: [try event("20", "turn_completed")])
        card.apply(events: [try event("21", "turn_accepted")])
        XCTAssertEqual(card.status, "Running")
        try card.apply(state: state("30", turns: []))
        XCTAssertEqual(card.status, "Idle", "New terminal history is still unknown")
        card.apply(events: [try event("28", "turn_failed"), try event("30", "event")])
        XCTAssertEqual(card.status, "Failed")
        XCTAssertTrue(card.needsAttention(seen: Cursor(rawValue: "20")))
        XCTAssertFalse(card.needsAttention(seen: Cursor(rawValue: "28")))
    }
    func testRunningSnapshotCannotReuseEarlierOutcomeWhenLaterWorkFinishes() throws {
        var card = AgentCard(id: "agent", title: "Test")
        try card.apply(state: state("10", turns: []))
        card.apply(events: [try event("10", "turn_completed")])
        try card.apply(state: state("20", turns: ["new"]))
        card.apply(events: [try event("10", "turn_completed")])
        XCTAssertEqual(card.status, "Running")
        try card.apply(state: state("30", turns: []))
        XCTAssertEqual(card.status, "Idle")
        card.apply(events: [try event("29", "turn_failed"), try event("30", "event")])
        XCTAssertEqual(card.status, "Failed")
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
    func testDeferredInboxCanReachZeroAndOnlyNewUpdatesReturn() throws {
        var card = AgentCard(id: "agent", title: "Test")
        try card.apply(state: state("20", turns: []))
        card.apply(events: [try event("20", "turn_completed", ["final_message": .string("Done")])])
        XCTAssertTrue(card.isInInbox(seen: nil, deferred: nil))
        XCTAssertFalse(card.isInInbox(seen: nil, deferred: Cursor(rawValue: "20")))
        var deck = InboxDeck()
        deck.reconcile([card.id]); deck.advance()
        deck.reconcile(card.isInInbox(seen: nil, deferred: Cursor(rawValue: "20")) ? [card.id] : [])
        XCTAssertNil(deck.focusedID)
        card.apply(events: [try event("21", "turn_accepted")])
        XCTAssertTrue(card.isInInbox(seen: nil, deferred: Cursor(rawValue: "20")))
        XCTAssertTrue(card.isInInbox(seen: nil, deferred: nil), "Previous restores a deferred card")
    }
    func testInboxWaitsForRealUpdatesAndKeepsInitialReadErrorsReachable() throws {
        var card = AgentCard(id: "agent", title: "Test")
        XCTAssertTrue(card.preview.isEmpty)
        XCTAssertFalse(card.isInInbox(seen: nil, deferred: nil), "An unloaded roster entry must not become a placeholder card")

        card.error = "Could not load this conversation"
        XCTAssertTrue(card.isInInbox(seen: nil, deferred: nil), "Failed reads remain available for retry")
        try card.apply(state: state("0", turns: []))
        XCTAssertFalse(card.isInInbox(seen: nil, deferred: nil), "An empty conversation does not occupy the inbox after retry")

        card.apply(events: [try event("1", "turn_accepted", ["input": .string("Check this")])])
        XCTAssertTrue(card.isInInbox(seen: Cursor(rawValue: "1"), deferred: nil), "Running work remains in the inbox even after it has been seen")
        card.apply(events: [try event("2", "turn_completed", ["final_message": .string("Done")])])
        XCTAssertEqual(card.preview, "Done")
        XCTAssertTrue(card.isInInbox(seen: Cursor(rawValue: "1"), deferred: nil))
        XCTAssertFalse(card.isInInbox(seen: Cursor(rawValue: "2"), deferred: nil))
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
    func testImageSubmissionUsesExistingManagedContentAndRetryIdentity() throws {
        let image: JSON = .object(["type": .string("image"), "image_url": .string("data:image/jpeg;base64,/9j/"), "detail": .string("high")])
        var command = AgentCommand(agentID: "agent", input: "Read this image", kind: .followUp, requestID: "with-image")
        command.images = [image]
        let spec = try command.requestSpec()
        XCTAssertEqual(spec.key, "inbox:with-image")
        XCTAssertEqual(spec.body?["id"].string, "with-image")
        XCTAssertEqual(spec.body?["input"], .array([.object(["type": .string("text"), "text": .string("Read this image")]), image]))
        XCTAssertEqual(try command.requestSpec().body, spec.body)
        var imageOnly = AgentCommand(agentID: "agent", kind: .followUp)
        imageOnly.images = [image]
        XCTAssertEqual(try imageOnly.requestSpec().body?["input"], .array([image]), "Do not send an empty text item")
        var oversized = command
        oversized.images = [.object(["type": .string("image"), "image_url": .string(String(repeating: "a", count: 1024 * 1024))])]
        XCTAssertThrowsError(try oversized.requestSpec()) { XCTAssertEqual($0 as? APIError, .messageTooLarge) }
    }
    func testImageHistoryProjectsPicturesSeparatelyFromMessageText() throws {
        let url = "data:image/png;base64,aGVsbG8="
        let input: JSON = .array([.object(["type": .string("text"), "text": .string("Describe this")]), .object(["type": .string("image"), "image_url": .string(url)])])
        let rows = transcript([try event("1", "turn_accepted", ["input": input])])
        XCTAssertEqual(rows.first?.text, "Describe this")
        XCTAssertEqual(rows.first?.images, [url])
        let old = Data(#"{"id":"old","role":"You","text":"Hello","detail":"","running":false}"#.utf8)
        XCTAssertNil(try JSONDecoder().decode(TranscriptRow.self, from: old).images)
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
    func testDefaultAgentCreationUsesNoBodyAndRetainsRetryIdentity() async throws {
        var requests = 0
        let fixture = try HTTPFixture { request in
            requests += 1
            XCTAssertEqual(request.method, "POST")
            XCTAssertEqual(request.path, "/v1/agents")
            XCTAssertTrue(request.body.isEmpty, "The service rejects {} for default creation")
            XCTAssertNil(request.headers["content-type"])
            XCTAssertNil(request.headers["cookie"])
            XCTAssertEqual(request.headers["idempotency-key"], "inbox-create-retry")
            if requests == 1 { return .init(status: 503) }
            return .init(status: 201, body: #"{"agent_id":"created-agent"}"#)
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        do { _ = try await client.create(requestID: "inbox-create-retry"); XCTFail("Failed creation accepted") }
        catch let error as APIError { XCTAssertEqual(error, .http(503)) }
        let id = try await client.create(requestID: "inbox-create-retry")
        XCTAssertEqual(id, "created-agent")
        XCTAssertEqual(requests, 2)
    }
    func testDeletionFenceIsDistinctFromRetriableTurnConflict() async throws {
        let fixture = try HTTPFixture { request in
            .init(status: 409, body: request.path.hasSuffix("deleting")
                ? #"{"error":"agent_deleting","message":"never expose server text"}"#
                : #"{"error":"turn_conflict"}"#)
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        do { _ = try await client.state("deleting"); XCTFail("Deletion fence accepted") }
        catch let error as APIError {
            XCTAssertEqual(error, .agentDeleting)
            XCTAssertFalse(error.localizedDescription.contains("never expose"))
        }
        do { _ = try await client.state("conflict"); XCTFail("Conflict accepted") }
        catch let error as APIError { XCTAssertEqual(error, .http(409)) }
    }
}
