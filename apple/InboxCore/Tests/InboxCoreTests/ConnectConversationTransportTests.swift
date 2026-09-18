#if canImport(Combine) && !os(Linux)
import XCTest
@testable import InboxCore

@MainActor final class ConnectConversationTransportTests: XCTestCase {
    private let grant = "0x" + String(repeating: "a", count: 64)
    private func authorization(origin: String = "https://connect.example") throws -> ConnectConversationAuthorization {
        try .init(origin: origin, grantID: grant, appID: "djbooth", appOrigin: "https://djbooth.example",
                  agentID: "booth-agent", bearerToken: "test-opaque-grant-token", capabilities: ["agent.history.read", "agent.output.final"])
    }

    func testRequestsStayOnApprovedGrantAndOrigin() throws {
        let receipt = try authorization()
        XCTAssertFalse(String(describing: receipt).contains("test-opaque-grant-token"))
        XCTAssertFalse(String(reflecting: receipt).contains("test-opaque-grant-token"))
        let transport = ConnectConversationTransport(authorization: receipt, title: "DJ Booth")
        defer { transport.close() }
        let request = try transport.request(agentID: "booth-agent", suffix: "/events/history?limit=128")
        XCTAssertEqual(request.url?.absoluteString, "https://connect.example/v1/grants/" + grant + "/agents/booth-agent/events/history?limit=128")
        XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer test-opaque-grant-token")
        XCTAssertEqual(request.value(forHTTPHeaderField: "Origin"), "https://djbooth.example")
        XCTAssertEqual(request.value(forHTTPHeaderField: "x-nanocodex-app-id"), "djbooth")
        XCTAssertEqual(transport.scope.conversations.map(\.id), ["booth-agent"])
        XCTAssertThrowsError(try transport.request(agentID: "other-project")) {
            XCTAssertEqual($0 as? APIError, .http(403))
        }
        XCTAssertThrowsError(try authorization(origin: "https://connect.example/redirect"))
        XCTAssertThrowsError(try ConnectConversationAuthorization(origin: "https://connect.example", grantID: grant,
            appID: "djbooth", appOrigin: "https://djbooth.example", agentID: "booth-agent",
            bearerToken: "test-token", capabilities: ["agent.output.final"])) {
            XCTAssertEqual($0 as? ConnectConversationError, .conversationVisibilityRequired)
        }
    }

    func testHistoryAndStableTurnUseTheRealConnectContract() async throws {
        var requests: [FixtureRequest] = []
        let fixture = try HTTPFixture { request in
            requests.append(request)
            if request.path.hasSuffix("/events/history") {
                return .init(body: #"{"data":[{"cursor":"4","type":"turn_completed","turn_id":"turn","final_message":"Ready"}],"has_more":false,"latest_cursor":"7"}"#)
            }
            return .init(body: #"{"turn_id":"stable-turn"}"#)
        }
        defer { fixture.close() }
        let transport = ConnectConversationTransport(authorization: try authorization(origin: fixture.origin),
                                                      title: "DJ Booth", configuration: fixture.configuration)
        defer { transport.close() }
        let history = try await transport.history("booth-agent", before: nil, after: Cursor(rawValue: "3"))
        XCTAssertEqual(history.latest.rawValue, "7")
        XCTAssertEqual(history.events.first?.cursor.rawValue, "4")
        let command = AgentCommand(agentID: "booth-agent", input: "Build a set", kind: .followUp, requestID: "stable-turn")
        try await transport.send(command)
        try await transport.send(command)
        XCTAssertEqual(requests.count, 3)
        XCTAssertEqual(requests[0].query, "limit=128&after=3")
        XCTAssertEqual(requests[1].path, "/v1/grants/" + grant + "/agents/booth-agent/turns")
        XCTAssertEqual(requests[1].body, requests[2].body)
        XCTAssertEqual(requests[1].headers["idempotency-key"], "inbox:stable-turn")
        do { try await transport.send(.init(agentID: "another-agent", input: "No", kind: .followUp)); XCTFail("Cross-grant write") }
        catch { XCTAssertEqual(error as? APIError, .http(403)) }
        XCTAssertEqual(requests.count, 3)
    }

    func testRevokedGrantIsReportedWithoutResubmitting() async throws {
        var count = 0
        let fixture = try HTTPFixture { _ in count += 1; return .init(status: 403, body: "{}") }
        defer { fixture.close() }
        let transport = ConnectConversationTransport(authorization: try authorization(origin: fixture.origin),
                                                      title: "DJ Booth", configuration: fixture.configuration)
        defer { transport.close() }
        do { try await transport.send(.init(agentID: "booth-agent", input: "Hello", kind: .followUp)); XCTFail("Revoked grant accepted") }
        catch { XCTAssertEqual(error as? APIError, .http(403)) }
        XCTAssertEqual(count, 1)
    }

    func testSSEPreservesProjectedCheckpointAndCancelsForegroundRead() async throws {
        let opened = expectation(description: "projected event delivered")
        var seen: [String] = []
        let fixture = try HTTPFixture { request in
            XCTAssertEqual(request.query, "cursor=3")
            XCTAssertEqual(request.headers["accept"], "text/event-stream")
            return .init(headers: ["Content-Type": "text/event-stream"],
                         body: "id: 4\ndata: {\"type\":\"turn_completed\",\"turn_id\":\"turn\",\"final_message\":\"Ready\"}\n\n: cursor 7\n\n",
                         streaming: true)
        }
        defer { fixture.close() }
        let transport = ConnectConversationTransport(authorization: try authorization(origin: fixture.origin),
                                                      title: "DJ Booth", configuration: fixture.configuration)
        defer { transport.close() }
        let task = Task {
            try await transport.stream("booth-agent", after: Cursor(rawValue: "3")!) { frame in
                await MainActor.run {
                    if let cursor = frame.cursor { seen.append(cursor.rawValue) }
                    if frame.cursor?.rawValue == "7" { opened.fulfill() }
                }
            }
        }
        await fulfillment(of: [opened], timeout: 3)
        task.cancel()
        do { try await task.value; XCTFail("Cancelled stream succeeded") }
        catch { XCTAssertTrue(error is CancellationError || (error as? URLError)?.code == .cancelled) }
        XCTAssertEqual(seen, ["4", "7"])
    }

    func testStopUsesCapturedTurnAndRejectsWrongAdmissionReceipt() async throws {
        var calls: [FixtureRequest] = []
        let fixture = try HTTPFixture { request in
            calls.append(request)
            return .init(body: #"{"turn_id":"other-turn"}"#)
        }
        defer { fixture.close() }
        let transport = ConnectConversationTransport(authorization: try authorization(origin: fixture.origin),
                                                      title: "DJ Booth", configuration: fixture.configuration)
        defer { transport.close() }
        do {
            try await transport.send(.init(agentID: "booth-agent", turnID: "running-turn", kind: .stop))
            XCTFail("Wrong receipt accepted")
        } catch { XCTAssertEqual(error as? APIError, .invalidResponse) }
        XCTAssertEqual(calls.count, 1)
        XCTAssertTrue(calls[0].path.hasSuffix("/turns/running%2Dturn/cancel") || calls[0].path.hasSuffix("/turns/running-turn/cancel"))
        XCTAssertEqual(calls[0].method, "POST")
    }

}
#endif
