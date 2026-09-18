import XCTest
@testable import InboxCore

final class MainThreadAPITests: XCTestCase {
    func testMainThreadReadAndEnsureUseAccountEndpoints() async throws {
        var requests: [FixtureRequest] = []
        let fixture = try HTTPFixture { request in
            requests.append(request)
            XCTAssertEqual(request.path, "/v1/main-thread")
            XCTAssertTrue(request.body.isEmpty)
            XCTAssertEqual(request.headers["authorization"], "Bearer " + fixtureKey)
            return request.method == "GET" ? FixtureReply(status: 404) : FixtureReply(body: #"{"agent_id":"main"}"#)
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        let absent = try await client.mainThread(ensure: false)
        let ensured = try await client.mainThread(ensure: true)
        XCTAssertNil(absent)
        XCTAssertEqual(ensured, "main")
        XCTAssertEqual(requests.map(\.method), ["GET", "PUT"])
    }

    func testCanonicalProjectListAndRegistrationDecodeCoordinator() async throws {
        var requests: [FixtureRequest] = []
        let fixture = try HTTPFixture { request in
            requests.append(request)
            if request.method == "GET" {
                XCTAssertEqual(request.path, "/v1/projects")
                return FixtureReply(body: #"{"data":[{"id":"project","name":"Shared","coordinator_agent_id":"root"}]}"#)
            }
            XCTAssertEqual(request.method, "PUT")
            XCTAssertEqual(request.path, "/v1/projects/project")
            XCTAssertEqual(request.json["name"] as? String, "Local")
            if request.json["coordinator_agent_id"] != nil {
                XCTAssertEqual(request.json["coordinator_agent_id"] as? String, "root")
            }
            XCTAssertNil(request.json["agentIDs"])
            return FixtureReply(body: #"{"id":"project","name":"Local","coordinator_agent_id":"root"}"#)
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        let projects = try await client.canonicalProjects()
        XCTAssertEqual(projects, [InboxProject(id: "project", name: "Shared", primaryAgentID: "root")])
        let registered = try await client.registerProject(id: "project", name: "Local", coordinatorAgentID: "root")
        XCTAssertEqual(registered, InboxProject(id: "project", name: "Local", primaryAgentID: "root"))
        _ = try await client.registerProject(id: "project", name: "Local", coordinatorAgentID: nil)
        XCTAssertNil(requests.last?.json["coordinator_agent_id"])
    }

    func testOverlongUnicodeNameRejectedBeforeCreatingProject() async throws {
        let fixture = try HTTPFixture { _ in
            XCTFail("Invalid names must not reach the service")
            return FixtureReply(status: 400)
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        do {
            _ = try await client.registerProject(id: "project", name: String(repeating: "😀", count: 81), coordinatorAgentID: nil)
            XCTFail("The backend counts UTF-16 code units")
        } catch APIError.invalidResponse {} catch { XCTFail("Unexpected error: \(error)") }
    }

    func testMalformedResponsesAndHTTPFailuresAreNotTreatedAsAbsence() async throws {
        for reply in [FixtureReply(body: "{}"), FixtureReply(body: #"{"agent_id":"../invalid"}"#),
                      FixtureReply(status: 503), FixtureReply(body: #"{"agent_id":false}"#)] {
            let fixture = try HTTPFixture { _ in reply }
            defer { fixture.close() }
            let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
            defer { client.close() }
            do { _ = try await client.mainThread(ensure: false); XCTFail("Expected rejection") } catch {}
            do { _ = try await client.canonicalProjects(); XCTFail("Expected rejection") } catch {}
        }
    }
}
