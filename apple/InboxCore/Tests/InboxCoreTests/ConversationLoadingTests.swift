import XCTest
@testable import InboxCore

final class ConversationLoadingTests: XCTestCase {
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

    func testUnreadableTailRecoveryIsBoundedAndCancellationStopsPaging() async throws {
        var count = 0
        let fixture = try HTTPFixture { _ in
            count += 1
            return .init(body: "{\"data\":[{\"cursor\":\"\(100 - count)\",\"type\":\"transport_status\"}],\"has_more\":true,\"latest_cursor\":\"100\"}", delay: 0.01)
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try .init(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        let history = try await client.conversationHistory("owned-agent")
        XCTAssertEqual(count, 4)
        XCTAssertTrue(history.hasMore)
        XCTAssertTrue(history.rows.isEmpty)
        let request = Task { try await client.conversationHistory("owned-agent") }
        request.cancel()
        do { _ = try await request.value; XCTFail("Cancelled history returned a window") }
        catch { XCTAssertTrue(error is CancellationError || (error as? URLError)?.code == .cancelled) }
    }
}
