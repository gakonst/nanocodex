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
}

private final class ConnectorRequestCapture: @unchecked Sendable {
    private let lock = NSLock()
    private var requests: [FixtureRequest] = []
    func append(_ request: FixtureRequest) { lock.lock(); requests.append(request); lock.unlock() }
    func snapshot() -> [FixtureRequest] { lock.lock(); defer { lock.unlock() }; return requests }
}
