import XCTest
@testable import InboxCore

final class ConversationLoadingTests: XCTestCase {
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
