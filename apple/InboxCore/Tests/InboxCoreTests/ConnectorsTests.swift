import Foundation
import XCTest
@testable import InboxCore

final class ConnectorsTests: XCTestCase {
    func testOverviewUsesServerCatalogAndDeduplicatesProviderAccounts() async throws {
        let connection = String(repeating: "a", count: 43)
        let fixture = try HTTPFixture { request in
            if request.path == "/v1/connectors/catalog" {
                return FixtureReply(body: #"{"providers":[{"id":"google","name":"Google Workspace","description":"Mail and files","capabilities":[{"id":"gmail","name":"Gmail"},{"id":"gdrive","name":"Google Drive"}]},{"id":"slack","name":"Slack","description":"Messages","capabilities":[{"id":"slack","name":"Slack"}]}]}"#)
            }
            if request.path == "/v1/connectors/mcp-connections" {
                return FixtureReply(body: #"{"mcp_connections":[{"id":"mmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmm","name":"Mercator","status":"connected"}]}"#)
            }
            return FixtureReply(body: #"{"connectors":{"gmail":{"connected":true,"connections":[{"id":"\#(connection)","label":"georgios@example.com","account_id":"google-1","capabilities":["gmail","gdrive"]}]},"gdrive":{"connected":true,"connections":[{"id":"\#(connection)","label":"georgios@example.com","account_id":"google-1","capabilities":["gmail","gdrive"]}]},"slack":{"connected":false,"connections":[]}}}"#)
        }
        defer { fixture.close() }
        let credential = try AccountCredential(origin: fixture.origin, apiKey: fixtureKey)
        let client = ManagedClient(credential: credential, configuration: fixture.configuration)
        defer { client.close() }

        let overview = try await client.connectorOverview()
        XCTAssertEqual(overview.providers.map(\.id), ["google", "slack"])
        let google = try XCTUnwrap(overview.providers.first)
        XCTAssertTrue(overview.isConnected(google))
        XCTAssertEqual(overview.connections(for: google), [ConnectorAccountConnection(
            id: connection,
            label: "georgios@example.com",
            accountID: "google-1",
            capabilities: ["gdrive", "gmail"]
        )])
        XCTAssertEqual(overview.capabilityNames(for: overview.connections(for: google)[0], provider: google), ["Google Drive", "Gmail"])
        XCTAssertEqual(overview.mcpConnections, [McpConnection(
            id: String(repeating: "m", count: 43),
            name: "Mercator",
            status: .connected
        )])
    }

    func testNativeAuthorizationUsesCorrelatedHTTPSCompletionAndExactRevoke() async throws {
        let capture = ConnectorRequestCapture()
        let connection = String(repeating: "b", count: 43)
        let fixture = try HTTPFixture { request in
            capture.append(request)
            if request.method == "POST" {
                return FixtureReply(body: #"{"authorization_url":"https://accounts.google.com/o/oauth2/v2/auth?client_id=client&state=state"}"#)
            }
            return FixtureReply(status: 204, body: "")
        }
        defer { fixture.close() }
        let credential = try AccountCredential(origin: fixture.origin, apiKey: fixtureKey)
        let client = ManagedClient(credential: credential, configuration: fixture.configuration)
        defer { client.close() }

        let authorization = try await client.beginConnectorAuthorization(provider: "google")
        XCTAssertEqual(authorization.authorizationURL.host, "accounts.google.com")
        XCTAssertEqual(authorization.callbackURL.scheme, "nanocodex")
        XCTAssertEqual(authorization.callbackURL.host, "connectors")
        XCTAssertEqual(authorization.callbackURL.path, "/complete")
        XCTAssertEqual(authorization.callbackURL.query, "attempt=\(authorization.attemptID)")
        let callback = URL(string: authorization.callbackURL.absoluteString
            + "&connector=google&connector_result=connected")!
        XCTAssertEqual(try authorization.result(from: callback), .connected)
        XCTAssertThrowsError(try authorization.result(from: URL(string: callback.absoluteString + "&code=private")!))

        try await client.disconnectConnector(provider: "google", connectionID: connection)
        let requests = capture.snapshot()
        XCTAssertEqual(requests.map(\.method), ["POST", "DELETE"])
        XCTAssertEqual(requests[0].path, "/v1/connectors/google")
        XCTAssertEqual(requests[0].json["return_to"] as? String,
                       "/v1/connectors/mobile-complete?attempt=\(authorization.attemptID)")
        XCTAssertEqual(requests[1].path, "/v1/connectors/google/connections/\(connection)")
        XCTAssertEqual(requests[0].headers["authorization"], "Bearer \(fixtureKey)")
    }

    func testPublicMcpAddsAndConnectsWithoutOAuth() async throws {
        let capture = ConnectorRequestCapture()
        let connection = String(repeating: "m", count: 43)
        let fixture = try HTTPFixture { request in
            capture.append(request)
            if request.path.hasSuffix("/start") {
                return FixtureReply(body: #"{"mcp_connection":{"id":"\#(connection)","name":"Mercator","status":"connected"}}"#)
            }
            return FixtureReply(status: 201, body: #"{"mcp_connection":{"id":"\#(connection)","name":"Mercator","status":"authorization_required"}}"#)
        }
        defer { fixture.close() }
        let credential = try AccountCredential(origin: fixture.origin, apiKey: fixtureKey)
        let client = ManagedClient(credential: credential, configuration: fixture.configuration)
        defer { client.close() }

        let added = try await client.addMcpConnection(target: "https://mercator.sh")
        XCTAssertEqual(added.name, "Mercator")
        let start = try await client.beginMcpAuthorization(connectionID: connection)
        XCTAssertEqual(start, .connected(McpConnection(
            id: connection,
            name: "Mercator",
            status: .connected
        )))
        let requests = capture.snapshot()
        XCTAssertEqual(requests.map(\.path), [
            "/v1/connectors/mcp-connections",
            "/v1/connectors/mcp-connections/\(connection)/start",
        ])
        XCTAssertEqual(requests[0].json["target"] as? String, "https://mercator.sh")
        let returnTo = try XCTUnwrap(requests[1].json["return_to"] as? String)
        XCTAssertNotNil(returnTo.range(
            of: #"^/v1/connectors/mcp-mobile-complete\?attempt=[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"#,
            options: .regularExpression
        ))
    }

    func testOAuthMcpUsesCorrelatedNativeCompletionAndExactRevoke() async throws {
        let capture = ConnectorRequestCapture()
        let connection = String(repeating: "o", count: 43)
        let fixture = try HTTPFixture { request in
            capture.append(request)
            if request.method == "POST" {
                return FixtureReply(body: #"{"mcp_connection":{"id":"\#(connection)","name":"Linear","status":"authorization_required"},"authorization_url":"https://mcp.linear.app/authorize?state=opaque","callback_state":"opaque"}"#)
            }
            return FixtureReply(status: 204, body: "")
        }
        defer { fixture.close() }
        let credential = try AccountCredential(origin: fixture.origin, apiKey: fixtureKey)
        let client = ManagedClient(credential: credential, configuration: fixture.configuration)
        defer { client.close() }

        let start = try await client.beginMcpAuthorization(connectionID: connection)
        guard case .authorization(let authorization) = start else { return XCTFail("Expected OAuth") }
        XCTAssertEqual(authorization.authorizationURL.host, "mcp.linear.app")
        XCTAssertEqual(authorization.callbackURL.absoluteString,
                       "nanocodex://connectors/mcp-complete?attempt=\(authorization.attemptID)")
        let callback = URL(string: authorization.callbackURL.absoluteString
            + "&mcp_connection=\(connection)&mcp_result=connected")!
        XCTAssertEqual(try authorization.result(from: callback), .connected)
        XCTAssertThrowsError(try authorization.result(from: URL(string: callback.absoluteString + "&code=private")!))

        try await client.disconnectMcpConnection(connectionID: connection)
        let requests = capture.snapshot()
        XCTAssertEqual(requests.map(\.method), ["POST", "DELETE"])
        XCTAssertEqual(requests[0].path, "/v1/connectors/mcp-connections/\(connection)/start")
        XCTAssertEqual(requests[1].path, "/v1/connectors/mcp-connections/\(connection)")
    }
}

private final class ConnectorRequestCapture: @unchecked Sendable {
    private let lock = NSLock()
    private var requests: [FixtureRequest] = []
    func append(_ request: FixtureRequest) { lock.lock(); requests.append(request); lock.unlock() }
    func snapshot() -> [FixtureRequest] { lock.lock(); defer { lock.unlock() }; return requests }
}
