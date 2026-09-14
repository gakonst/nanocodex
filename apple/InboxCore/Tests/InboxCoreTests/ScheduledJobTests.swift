import Foundation
import XCTest
import InboxCore

final class ScheduledJobTests: XCTestCase {
    private let payload = #"{"id":"daily-check","cron":"0 9 * * *","timezone":"Europe/Athens","input":"Check the forecast","enabled":true,"session_mode":"new","next_run_at":1788760800000,"last_run_at":1788674400000,"last_skipped_at":null,"last_agent_id":"run-agent","last_turn_id":"cron:revision:1788674400000"}"#

    func testAccountDiscoverySkipsOnlyExplicitlyEmptyAgentsAndSupportsOlderServers() async throws {
        let fixture = try HTTPFixture { request in
            XCTAssertEqual(request.path, "/v1/agents")
            return FixtureReply(body: #"{"data":["empty","scheduled","legacy","unknown"],"summaries":{"empty":{"may_have_scheduled_jobs":false},"scheduled":{"may_have_scheduled_jobs":true},"unknown":{"may_have_scheduled_jobs":"false"}}}"#)
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        let agents = try await client.list()
        XCTAssertEqual(agents.filter(\.mayHaveScheduledJobs).map(\.id), ["scheduled", "legacy", "unknown"])
        XCTAssertTrue(AgentCard(id: "legacy", title: "Older server").mayHaveScheduledJobs)
    }

    func testListUsesAuthenticatedReadAndPreservesScheduleAndRunIdentity() async throws {
        let fixture = try HTTPFixture { [payload] request in
            XCTAssertEqual(request.path, "/v1/agents/source-agent/triggers")
            XCTAssertEqual(request.method, "GET")
            XCTAssertEqual(request.headers["authorization"], "Bearer " + fixtureKey)
            XCTAssertTrue(request.body.isEmpty)
            return FixtureReply(body: "{\"data\":[" + payload + "]}")
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        let jobs = try await client.scheduledJobs("source-agent")
        let job = try XCTUnwrap(jobs.first)
        XCTAssertEqual(jobs.count, 1)
        XCTAssertEqual(job.agentID, "source-agent")
        XCTAssertEqual(job.lastRunAgentID, "run-agent")
        XCTAssertEqual(job.cron, "0 9 * * *")
        XCTAssertEqual(job.timezone, "Europe/Athens")
        XCTAssertTrue(job.startsNewConversation)
        XCTAssertEqual(job.nextRun?.timeIntervalSince1970, 1_788_760_800)
        XCTAssertEqual(job.lastRun?.timeIntervalSince1970, 1_788_674_400)
        XCTAssertNil(job.lastSkipped)
        let other = try ScheduledJob(JSONDecoder().decode(JSON.self, from: Data(payload.utf8)), agentID: "other-agent")
        XCTAssertNotEqual(job.id, other.id, "Trigger IDs are scoped to the owning agent")
    }

    func testPausedLegacyScheduleAndMalformedFields() throws {
        var fields = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(payload.utf8)) as? [String: Any])
        fields["enabled"] = false; fields["next_run_at"] = NSNull()
        fields.removeValue(forKey: "session_mode"); fields.removeValue(forKey: "last_agent_id")
        func decode(_ fields: [String: Any]) throws -> ScheduledJob {
            try ScheduledJob(JSONDecoder().decode(JSON.self, from: JSONSerialization.data(withJSONObject: fields)), agentID: "source-agent")
        }
        let paused = try decode(fields)
        XCTAssertFalse(paused.enabled)
        XCTAssertFalse(paused.startsNewConversation)
        XCTAssertNil(paused.nextRun)
        XCTAssertEqual(paused.lastRunAgentID, "source-agent")
        for (key, bad) in [("id", "../escape"), ("enabled", "false"), ("session_mode", "invalid"), ("last_agent_id", "../other")] {
            var malformed = fields; malformed[key] = bad
            XCTAssertThrowsError(try decode(malformed), key)
        }
        for bad: Any in [-1, 1.5, "1788760800000"] {
            var malformed = fields; malformed["next_run_at"] = bad
            XCTAssertThrowsError(try decode(malformed))
        }
    }

    func testFailedOrMalformedListingIsNotAnEmptyScheduleList() async throws {
        for (status, body) in [(401, "{}"), (403, "{}"), (200, "{}"), (200, "{\"data\":[" + payload + "," + payload + "]}")] {
            let fixture = try HTTPFixture { _ in FixtureReply(status: status, body: body) }
            defer { fixture.close() }
            let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
            defer { client.close() }
            do { _ = try await client.scheduledJobs("source-agent"); XCTFail("Expected listing failure") }
            catch let error as APIError { XCTAssertEqual(error, status == 200 ? .invalidResponse : .http(status)) }
        }
    }

    func testAccountReadsDeliverJobsAndFailuresWhileOneAgentIsStillWaiting() async throws {
        let slowResponse = DispatchGroup(); slowResponse.enter()
        let lastFastResult = expectation(description: "Agents beyond the first four finish before the slow response")
        let received = ScheduleReadResults()
        var requests: [String] = []
        let fixture = try HTTPFixture { [payload] request in
            requests.append(request.path)
            XCTAssertEqual(request.method, "GET")
            XCTAssertEqual(request.headers["authorization"], "Bearer " + fixtureKey)
            if request.path == "/v1/agents/agent-2/triggers" { return FixtureReply(status: 503) }
            return FixtureReply(body: "{\"data\":[" + payload + "]}",
                gate: request.path == "/v1/agents/agent-0/triggers" ? slowResponse : nil)
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        let ids = (0..<9).map { "agent-\($0)" }
        let task = Task {
            await client.scheduledJobs(for: ids + ["agent-1"]) { id, result in
                await received.append(id, result)
                if id == "agent-8" { lastFastResult.fulfill() }
            }
        }
        await fulfillment(of: [lastFastResult], timeout: 5)
        let partial = await received.snapshot()
        XCTAssertTrue(partial.succeeded.contains("agent-8"))
        XCTAssertFalse(partial.succeeded.contains("agent-0"), "Rows arrive before the slow response is released")
        slowResponse.leave()
        await task.value
        let all = await received.snapshot()
        XCTAssertEqual(all.succeeded.sorted(), ids.filter { $0 != "agent-2" }.sorted())
        XCTAssertEqual(all.failed, ["agent-2"])
        XCTAssertEqual(fixture.queue.sync { requests.count }, 9, "Duplicate owners must not cause duplicate API calls")
    }

    func testAccountReadsBoundConcurrencyAndCancelWithoutPublishingOrStartingMoreReads() async throws {
        let responses = DispatchGroup(); responses.enter()
        let firstFour = expectation(description: "Four reads in flight"); firstFour.expectedFulfillmentCount = 4
        let excessRead = expectation(description: "No fifth read before a slot opens"); excessRead.isInverted = true
        let received = ScheduleReadResults()
        var count = 0
        let fixture = try HTTPFixture { _ in
            count += 1
            if count <= 4 { firstFour.fulfill() } else { excessRead.fulfill() }
            return FixtureReply(body: "{\"data\":[]}", gate: responses)
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        let task = Task {
            await client.scheduledJobs(for: (0..<20).map { "agent-\($0)" }) { id, result in
                await received.append(id, result)
            }
        }
        await fulfillment(of: [firstFour], timeout: 5)
        await fulfillment(of: [excessRead], timeout: 0.1)
        task.cancel()
        responses.leave()
        await task.value
        let all = await received.snapshot()
        XCTAssertTrue(all.succeeded.isEmpty)
        XCTAssertTrue(all.failed.isEmpty)
        XCTAssertEqual(fixture.queue.sync { count }, 4)
    }

    func testMissingSchedulesRequireFreshRosterConfirmation() async throws {
        var rosterReads = 0
        let fixture = try HTTPFixture { request in
            if request.path == "/v1/agents" {
                rosterReads += 1
                return FixtureReply(body: #"{"data":["still-present"],"summaries":{}}"#)
            }
            return FixtureReply(status: 404)
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        let received = ScheduleReadResults()
        await client.scheduledJobs(for: ["deleted", "also-deleted", "still-present"]) { id, result in
            await received.append(id, result)
        }
        let all = await received.snapshot()
        XCTAssertEqual(all.succeeded.sorted(), ["also-deleted", "deleted"])
        XCTAssertEqual(all.failed, ["still-present"])
        XCTAssertEqual(fixture.queue.sync { rosterReads }, 1)
    }

    func testMissingSchedulesDoNotHideFailedRosterAuthorization() async throws {
        let fixture = try HTTPFixture { request in
            FixtureReply(status: request.path == "/v1/agents" ? 401 : 404)
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        await client.scheduledJobs(for: ["deleted"]) { _, result in
            guard case .failure(let error) = result else { return XCTFail("Authorization failure must not become an empty schedule list") }
            XCTAssertEqual(error as? APIError, .http(401))
        }
    }

    func testCancellingRosterConfirmationDoesNotPublishEmptySchedules() async throws {
        let releaseRoster = DispatchGroup(); releaseRoster.enter()
        let rosterStarted = expectation(description: "Confirming missing owner against the account roster")
        let fixture = try HTTPFixture { request in
            if request.path == "/v1/agents" {
                rosterStarted.fulfill()
                return FixtureReply(body: #"{"data":[],"summaries":{}}"#, gate: releaseRoster)
            }
            return FixtureReply(status: 404)
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        let received = ScheduleReadResults()
        let task = Task {
            await client.scheduledJobs(for: ["deleted"]) { id, result in await received.append(id, result) }
        }
        await fulfillment(of: [rosterStarted], timeout: 5)
        task.cancel(); releaseRoster.leave()
        await task.value
        let all = await received.snapshot()
        XCTAssertTrue(all.succeeded.isEmpty)
        XCTAssertTrue(all.failed.isEmpty)
    }
}

private actor ScheduleReadResults {
    private var succeeded: [String] = []
    private var failed: [String] = []
    func append(_ id: String, _ result: Result<[ScheduledJob], Error>) {
        switch result {
        case .success(let jobs):
            XCTAssertTrue(jobs.allSatisfy { $0.agentID == id })
            succeeded.append(id)
        case .failure: failed.append(id)
        }
    }
    func snapshot() -> (succeeded: [String], failed: [String]) { (succeeded, failed) }
}
