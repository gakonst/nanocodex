import XCTest
@testable import InboxCore

final class ConversationLoadingTests: XCTestCase {
    func testToolOnlyTailAtCursor8600ReturnsLatestPageAndPreparedProjection() async throws {
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
        XCTAssertEqual(requests, 1, "Opening must not wait for older pages")
        XCTAssertEqual(history.events.count, 128)
        XCTAssertEqual(history.rows.count, 128)
        XCTAssertEqual(history.events.first?.cursor.rawValue, "8473")
        XCTAssertEqual(history.events.last?.cursor.rawValue, "8600")
        XCTAssertEqual(history.latest.rawValue, "8600")
        XCTAssertTrue(history.hasMore)
        XCTAssertFalse(history.hasNewer)
        XCTAssertEqual(history.byteCounts.count, history.events.count)
        let replay = try await history.projector.rows(history.events)
        XCTAssertEqual(replay, history.rows)
        print("TOOL_TAIL_OPENING_PERF tail=8600 retained=128 pages=1 opening=\(opening)")
    }

    func testOpeningByteLimitRetainsNewestEventsAndKeepsEvictedHistoryReachable() async throws {
        let largeResult = String(repeating: "x", count: 9 * 1024 * 1024)
        func tool(_ cursor: Int) -> JSON {
            .object(["cursor": .string(String(cursor)), "type": .string("event"), "turn_id": .string("t"),
                "event": .object(["type": .string("tool.result"), "payload": .object([
                    "tool": .string("web.run"), "call_id": .string("call-\(cursor)"),
                    "result": .object(["output": .string(largeResult)])])])])
        }
        let body = try JSONEncoder().encode(JSON.object([
            "data": .array([tool(1), tool(2)]), "has_more": .bool(false), "latest_cursor": .string("3")]))
        var requests = 0
        let fixture = try HTTPFixture { _ in
            requests += 1
            return .init(body: String(decoding: body, as: UTF8.self))
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try .init(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        let history = try await client.conversationHistory("synthetic-agent")
        XCTAssertEqual(requests, 1)
        XCTAssertEqual(history.events.map { $0.cursor.rawValue }, ["2"])
        XCTAssertEqual(history.rows.count, 1)
        XCTAssertTrue(history.hasMore, "Locally evicted events must remain available to backward paging")
        XCTAssertFalse(history.hasNewer, "Opening must retain the newest edge")
        XCTAssertEqual(history.latest.rawValue, "3", "Use the server snapshot cursor, not the last retained event")
        XCTAssertEqual(history.byteCounts.count, history.events.count)
        XCTAssertLessThanOrEqual(history.byteCounts.reduce(0, +), 16 * 1024 * 1024)
        let replay = try await history.projector.rows(history.events)
        XCTAssertEqual(replay, history.rows)
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
    func testTransportOnlyTailReturnsBeforeExplicitBackfillAndPreservesReplayCursor() async throws {
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
        XCTAssertEqual(requests, ["limit=128"])
        XCTAssertTrue(history.rows.isEmpty)
        XCTAssertEqual(history.events.map { $0.cursor.rawValue }, ["200"])
        XCTAssertTrue(history.hasMore)
        XCTAssertFalse(history.hasNewer)
        XCTAssertEqual(history.byteCounts.count, history.events.count)
        let older = try await client.history("owned-agent", before: history.events.first!.cursor)
        XCTAssertEqual(requests, ["limit=128", "limit=128&before=200"])
        XCTAssertEqual(older.latest.rawValue, "205")
        XCTAssertEqual(history.latest.rawValue, "200", "New events discovered by a later history read must still replay")
        let rows = try await history.projector.rows(older.events + history.events)
        XCTAssertEqual(rows.map(\.text), ["Hello", "Reply"])
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

    func testEmptyTerminalTailReturnsItsReplayCursor() async throws {
        let fixture = try HTTPFixture { _ in
            .init(body: #"{"data":[],"has_more":false,"latest_cursor":"100"}"#)
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try .init(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        let history = try await client.conversationHistory("owned-agent")
        XCTAssertTrue(history.events.isEmpty)
        XCTAssertTrue(history.rows.isEmpty)
        XCTAssertTrue(history.byteCounts.isEmpty)
        XCTAssertEqual(history.latest.rawValue, "100")
        XCTAssertFalse(history.hasMore)
        XCTAssertFalse(history.hasNewer)
    }

    func testEmptyTailAdvertisingOlderHistoryFailsWithoutPaging() async throws {
        var requests = 0
        let fixture = try HTTPFixture { _ in
            requests += 1
            return .init(body: #"{"data":[],"has_more":true,"latest_cursor":"100"}"#)
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try .init(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        do { _ = try await client.conversationHistory("owned-agent"); XCTFail("Missing backward boundary accepted") }
        catch { XCTAssertEqual(error as? APIError, .invalidResponse) }
        XCTAssertEqual(requests, 1)
    }

    func testCancelledOpeningDoesNotReturnAWindow() async throws {
        let fixture = try HTTPFixture { _ in
            .init(body: #"{"data":[{"cursor":"100","type":"transport_status"}],"has_more":true,"latest_cursor":"100"}"#, delay: 0.01)
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try .init(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        let request = Task { try await client.conversationHistory("owned-agent") }
        request.cancel()
        do { _ = try await request.value; XCTFail("Cancelled history returned a window") }
        catch { XCTAssertTrue(error is CancellationError || (error as? URLError)?.code == .cancelled) }
    }
}
