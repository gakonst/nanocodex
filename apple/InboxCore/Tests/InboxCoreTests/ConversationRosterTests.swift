import XCTest
@testable import InboxCore

final class ConversationRosterTests: XCTestCase {
    func testRosterKeepsEveryConversationIndependentWithItsOwnTitle() async throws {
        let fixture = try HTTPFixture { request in
            XCTAssertEqual(request.path, "/v1/agents")
            return .init(body: #"{"data":["alpha","beta","untitled"],"summaries":{"alpha":{"title":"Plan the release","updated_at":10,"turn_count":2,"project_title":"Legacy title","project_name":"Legacy group","project_root_id":"alpha"},"beta":{"title":"Review the design","updated_at":20,"turn_count":1,"project_title":"Legacy title","project_name":"Legacy group","project_root_id":"alpha","parent_agent_id":"alpha","origin_turn_id":"earlier","project_turn_id":"task"},"untitled":{"title":"","turn_count":0}}}"#)
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try .init(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }

        let cards = try await client.list()
        XCTAssertEqual(cards.map(\.id), ["alpha", "beta", "untitled"])
        XCTAssertEqual(cards.map(\.title), ["Plan the release", "Review the design", "Untitled agent"])
        XCTAssertEqual(cards.sorted(by: AgentCard.mostRecentFirst).map(\.id), ["beta", "alpha", "untitled"])
    }
}
