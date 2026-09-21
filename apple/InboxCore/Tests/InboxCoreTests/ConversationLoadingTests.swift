import XCTest
@testable import InboxCore

final class ConversationLoadingTests: XCTestCase {
    func testOpeningConversationCandidatesMatchProjectionIncludingHiddenVoiceInputs() throws {
        let values: [JSON] = [
            .object(["type": .string("turn_accepted"), "input": .string("Hello")]),
            .object(["type": .string("turn_accepted"), "input": .string("<startup_context>internal</startup_context>")]),
            .object(["type": .string("turn_accepted"), "input": .string("<realtime_delegation><source>tail_flush</source></realtime_delegation>")]),
            .object(["type": .string("turn_accepted"), "input": .string("<realtime_delegation><transcript_delta>user: Hello\nassistant: Welcome</transcript_delta></realtime_delegation>")]),
            .object(["type": .string("turn_accepted"), "input": .string("<realtime_delegation><source>voice_bootstrap</source><input>Hello</input></realtime_delegation>")]),
            .object(["type": .string("turn_completed"), "final_message": .string("")]),
            .object(["type": .string("turn_completed"), "final_message": .string("Answer")])
        ] + ["assistant.delta", "assistant.message", "reasoning.summary.delta", "tool.call", "tool.result", "run.error"].map { type in
            .object(["type": .string("event"), "event": .object(["type": .string(type), "payload": .object(["text": .string("text")])])])
        }
        for (index, value) in values.enumerated() {
            let event = try AgentEvent(value, cursor: String(index))
            XCTAssertEqual(event.producesConversationRow, transcript([event]).contains { $0.role == "You" || $0.role == "Agent" })
        }
    }

    func testToolOnlyTailAtCursor8600FindsOpeningAndHandsOffCompleteProjection() async throws {
        var requests = 0
        let fixture = try HTTPFixture { request in
            requests += 1
            let before = request.query?.components(separatedBy: "before=").last.flatMap(Int.init) ?? 8601
            let lower = max(1, before - 128)
            let events: [JSON] = (lower..<before).map { cursor in
                if cursor == 1 {
                    return .object(["cursor": .string("1"), "type": .string("turn_accepted"), "turn_id": .string("t"), "input": .string("Investigate synthetic tools")])
                }
                return .object(["cursor": .string(String(cursor)), "type": .string("event"), "turn_id": .string("t"),
                    "event": .object(["type": .string("tool.result"), "payload": .object([
                        "tool": .string("web.run"), "call_id": .string("call-\(cursor)"),
                        "result": .object(["output": .string("Synthetic result")])])])])
            }
            let body = try! JSONEncoder().encode(JSON.object(["data": .array(events), "has_more": .bool(lower > 1), "latest_cursor": .string("8600")]))
            return .init(body: String(decoding: body, as: UTF8.self))
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try .init(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        let start = ContinuousClock.now
        let history = try await client.conversationHistory("synthetic-agent")
        let opening = start.duration(to: .now)
        XCTAssertEqual(requests, 68)
        XCTAssertEqual(history.events.count, 8600)
        XCTAssertEqual(history.rows.count, 8600)
        XCTAssertEqual(history.rows.first?.text, "Investigate synthetic tools")
        XCTAssertEqual(history.latest.rawValue, "8600")
        XCTAssertFalse(history.hasMore)
        XCTAssertFalse(history.hasNewer)
        XCTAssertEqual(history.byteCounts.count, history.events.count)
        let replay = try await history.projector.rows(history.events)
        XCTAssertEqual(replay, history.rows)
        print("TOOL_TAIL_OPENING_PERF events=8600 pages=68 opening=\(opening)")
    }

    func testOpeningProjectionHandoffPerformanceAndStreamCorrectness() async throws {
        // A full 128-event page with tool results and readable text, no private data.
        let events: [JSON] = (1...128).map { index in
            let turn = "turn-\(index / 4)"
            if index % 4 == 0 {
                return .object(["cursor": .string("\(index)"), "type": .string("turn_completed"),
                    "turn_id": .string(turn), "final_message": .string(String(repeating: "Synthetic answer. ", count: 128))])
            }
            return .object(["cursor": .string("\(index)"), "type": .string("event"), "turn_id": .string(turn),
                "event": .object(["type": .string("tool.result"), "payload": .object([
                    "tool": .string("web.run"), "call_id": .string("call-\(index)"),
                    "result": .object(["content": .array([.object(["type": .string("text"),
                        "text": .string(String(repeating: "Synthetic search result. ", count: 1024))])])])])])])
        }
        let body = try JSONEncoder().encode(JSON.object(["data": .array(events),
            "has_more": .bool(true), "latest_cursor": .string("128")]))
        let fixture = try HTTPFixture { _ in .init(body: String(decoding: body, as: UTF8.self)) }
        defer { fixture.close() }
        let client = ManagedClient(credential: try .init(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        let start = ContinuousClock.now
        let history = try await client.conversationHistory("synthetic-agent")
        let opening = start.duration(to: .now)
        let coldStart = ContinuousClock.now
        let cold = try await TranscriptStreamProjection().rows(history.events)
        let coldTime = coldStart.duration(to: .now)
        XCTAssertEqual(cold, history.rows)
        let replayStart = ContinuousClock.now
        let projector = history.projector
        let replay = try await projector.rows(history.events)
        let replayTime = replayStart.duration(to: .now)
        XCTAssertEqual(replay, history.rows)
        let next = try AgentEvent(.object(["cursor": .string("129"), "type": .string("turn_completed"),
            "turn_id": .string("new-turn"), "final_message": .string("New stream answer")]))
        let resumed = try await projector.rows(history.events + [next])
        XCTAssertEqual(resumed, transcript(history.events + [next]))
        let older = try AgentEvent(.object(["cursor": .string("0"), "type": .string("turn_accepted"),
            "turn_id": .string("earlier"), "input": .string("Earlier request")]))
        let prepended = try await projector.rows([older] + history.events)
        XCTAssertEqual(prepended, transcript([older] + history.events))
        print("OPENING_HANDOFF_PERF events=128 payload_bytes=\(body.count) opening=\(opening) old_first_stream_projection=\(coldTime) transferred_first_stream_projection=\(replayTime)")
    }

    func testHistoryResponsePreservesAnAnswerLargerThanTheOldResponseAndWindowCaps() async throws {
        let answer = String(repeating: "x", count: 33 * 1024 * 1024) + " full answer"
        let fixture = try HTTPFixture { _ in
            .init(body: "{\"data\":[{\"cursor\":\"1\",\"type\":\"turn_completed\",\"turn_id\":\"t\",\"final_message\":\"" + answer + "\"}],\"has_more\":false,\"latest_cursor\":\"1\"}")
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try .init(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        let history = try await client.conversationHistory("owned-agent")
        XCTAssertEqual(history.rows.first?.text, answer)
        XCTAssertEqual(history.events.count, 1)
        XCTAssertFalse(history.hasMore)
        XCTAssertFalse(history.hasNewer)
    }
    func testTransportOnlyTailFindsMessagesWithoutAdvancingReplayCursor() async throws {
        var requests: [String] = []
        let fixture = try HTTPFixture { request in
            requests.append(request.query ?? "")
            if request.query?.contains("before=200") == true {
                return .init(body: #"{"data":[{"cursor":"198","type":"turn_accepted","turn_id":"t","input":"Hello"},{"cursor":"199","type":"turn_completed","turn_id":"t","final_message":"Reply"}],"has_more":false,"latest_cursor":"205"}"#)
            }
            return .init(body: #"{"data":[{"cursor":"200","type":"transport_status"}],"has_more":true,"latest_cursor":"200"}"#)
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try .init(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        let history = try await client.conversationHistory("owned-agent")
        XCTAssertEqual(requests, ["limit=128", "limit=128&before=200"])
        XCTAssertEqual(history.rows.map(\.text), ["Hello", "Reply"])
        XCTAssertEqual(history.latest.rawValue, "200", "New events discovered by a later history read must still replay")
        XCTAssertFalse(history.hasMore)
        XCTAssertEqual(history.byteCounts.count, history.events.count)
    }

    func testOpeningReadableHistoryDoesNotFetchOlderPagesOrState() async throws {
        var paths: [String] = []
        let fixture = try HTTPFixture { request in
            paths.append(request.path)
            return .init(body: #"{"data":[{"cursor":"10","type":"turn_completed","turn_id":"t","final_message":"Already readable"}],"has_more":true,"latest_cursor":"10"}"#)
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try .init(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        let history = try await client.conversationHistory("owned-agent")
        XCTAssertEqual(paths, ["/v1/agents/owned-agent/events/history"])
        XCTAssertEqual(history.rows.first?.text, "Already readable")
        XCTAssertTrue(history.hasMore)
    }

    func testUnreadableTailRecoveryContinuesBeyondFourPagesAndCancellationStopsPaging() async throws {
        var count = 0
        let fixture = try HTTPFixture { _ in
            count += 1
            if count == 9 { return .init(body: #"{"data":[{"cursor":"91","type":"turn_completed","turn_id":"t","final_message":"Found beyond the old cutoff"}],"has_more":false,"latest_cursor":"100"}"#) }
            return .init(body: "{\"data\":[{\"cursor\":\"\(100 - count)\",\"type\":\"transport_status\"}],\"has_more\":true,\"latest_cursor\":\"100\"}", delay: 0.01)
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try .init(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        let history = try await client.conversationHistory("owned-agent")
        XCTAssertEqual(count, 9)
        XCTAssertFalse(history.hasMore)
        XCTAssertEqual(history.rows.first?.text, "Found beyond the old cutoff")
        let request = Task { try await client.conversationHistory("owned-agent") }
        request.cancel()
        do { _ = try await request.value; XCTFail("Cancelled history returned a window") }
        catch { XCTAssertTrue(error is CancellationError || (error as? URLError)?.code == .cancelled) }
    }
}
