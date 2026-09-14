import Foundation
import XCTest
import InboxCore

final class AgentRefreshTests: XCTestCase {
    func testRefreshPriorityKeepsAllAgentsAndStableOrderWithinEachPriority() {
        var cards = ["idle-a", "error", "running", "pending-a", "voice", "focused", "pending-b", "idle-b"]
            .map { AgentCard(id: $0, title: $0) }
        cards[1].error = "Read failed"
        cards[2].activeTurns = ["remote-turn"]
        cards[4].activeTurns = ["voice-turn"]
        let order = AgentCard.refreshOrder(cards, focusedID: "focused", voiceID: "voice", pendingIDs: ["pending-a", "pending-b"])
        XCTAssertEqual(order, ["focused", "voice", "pending-a", "pending-b", "running", "error", "idle-a", "idle-b"])
        XCTAssertEqual(Set(order), Set(cards.map(\.id)))
        XCTAssertEqual(AgentCard.refreshOrder(Array(cards.suffix(2)), focusedID: nil, voiceID: nil, pendingIDs: []), ["pending-b", "idle-b"])
    }

    func testOverviewAppliedHistoryCursorExcludesUnreadStateAndStaleReplay() throws {
        var card = AgentCard(id: "overview", title: "Overview")
        func state(_ cursor: String) throws -> JSON { try JSONDecoder().decode(JSON.self, from: Data(refreshState("overview", cursor: cursor).utf8)) }
        func event(_ cursor: String) throws -> AgentEvent {
            try AgentEvent(JSONDecoder().decode(JSON.self, from: Data("{\"type\":\"turn_completed\",\"cursor\":\"\(cursor)\",\"turn_id\":\"turn\",\"final_message\":\"Done\"}".utf8)))
        }
        try card.apply(state: state("20"))
        XCTAssertEqual(card.appliedHistoryCursor, .zero, "A state cursor must not mark unseen history as read")
        card.apply(events: [try event("19")], transcriptRows: [])
        XCTAssertEqual(card.appliedHistoryCursor, Cursor(rawValue: "19"))
        try card.apply(state: state("30"))
        XCTAssertEqual(card.appliedHistoryCursor, Cursor(rawValue: "19"))
        card.apply(events: [try event("18")], transcriptRows: [])
        XCTAssertEqual(card.appliedHistoryCursor, Cursor(rawValue: "19"), "Older refresh responses cannot regress the overview cursor")
        card.apply(events: [try event("30")], transcriptRows: [])
        XCTAssertEqual(card.appliedHistoryCursor, Cursor(rawValue: "30"))
    }

    func testStragglerDoesNotDelayCompletedCardsOrLaterAgents() async throws {
        let started = ContinuousClock.now
        let received = RefreshResults(started: started)
        var requests: [String] = []
        let fixture = try HTTPFixture { request in
            requests.append(request.path)
            XCTAssertEqual(request.headers["authorization"], "Bearer " + fixtureKey)
            let id = String(request.path.split(separator: "/").last!)
            return FixtureReply(status: id == "agent-2" ? 503 : 200,
                body: refreshState(id), delay: id == "agent-0" ? 0.45 : 0.02)
        }
        defer { fixture.close() }
        let client = try refreshClient(fixture)
        defer { client.close() }
        let ids = (0..<9).map { "agent-\($0)" }
        await client.refreshAgents(ids + ["agent-1"], history: { _ in .stateOnly }) { id, result in
            await received.append(id, result)
        }
        let values = await received.values
        XCTAssertEqual(values.count, ids.count)
        XCTAssertEqual(Set(values.keys), Set(ids), "No agent is skipped, including failed reads")
        XCTAssertEqual(values["agent-2"]?.error as? APIError, .http(503))
        let first = try XCTUnwrap(values.values.map(\.milliseconds).min())
        let later = try XCTUnwrap(values["agent-8"]?.milliseconds)
        let slow = try XCTUnwrap(values["agent-0"]?.milliseconds)
        print("REFRESH_STRAGGLER first_callback_ms=\(first) last_fast_ms=\(later) slow_ms=\(slow) agents=\(values.count)")
        XCTAssertLessThan(later, slow, "An agent beyond the first four must publish before the straggler")
        XCTAssertEqual(fixture.queue.sync { requests.count }, ids.count, "IDs are deduplicated")
    }

    func testFourOperationsAndCancellationStopCallbacksAndAdditionalReads() async throws {
        let gate = DispatchGroup(); gate.enter()
        let four = expectation(description: "Four reads started"); four.expectedFulfillmentCount = 4
        let fifth = expectation(description: "No fifth operation starts before a slot opens"); fifth.isInverted = true
        var count = 0
        let received = RefreshResults()
        let fixture = try HTTPFixture { request in
            count += 1
            if count <= 4 { four.fulfill() } else { fifth.fulfill() }
            return FixtureReply(body: refreshState(String(request.path.split(separator: "/").last!)), gate: gate)
        }
        defer { fixture.close() }
        let client = try refreshClient(fixture)
        defer { client.close() }
        let task = Task {
            await client.refreshAgents((0..<20).map { "agent-\($0)" }, history: { _ in .stateOnly }) { id, result in
                await received.append(id, result)
            }
        }
        await fulfillment(of: [four], timeout: 5)
        await fulfillment(of: [fifth], timeout: 0.1)
        task.cancel(); gate.leave()
        await task.value
        let values = await received.values
        XCTAssertTrue(values.isEmpty)
        XCTAssertEqual(fixture.queue.sync { count }, 4)
        print("REFRESH_CONCURRENCY max_blocked_operations=\(fixture.queue.sync { count }) callbacks_after_cancel=\(values.count)")
    }

    func testInitialAndChangedHistoryPoliciesRetainAuthoritativeErrors() async throws {
        var requests: [String] = []
        let received = RefreshResults()
        let fixture = try HTTPFixture { request in
            requests.append(request.path)
            let id = String(request.path.split(separator: "/")[2])
            if id == "unauthorized" { return FixtureReply(status: 401) }
            if id == "deleted" { return FixtureReply(status: 409, body: #"{"error":"agent_deleting"}"#) }
            if request.path.hasSuffix("/events/history") {
                if id == "history-error" { return FixtureReply(status: 403) }
                return FixtureReply(body: #"{"data":[],"has_more":false,"latest_cursor":"2"}"#)
            }
            return FixtureReply(body: refreshState(id, cursor: id == "unchanged" ? "1" : "2"))
        }
        defer { fixture.close() }
        let client = try refreshClient(fixture)
        defer { client.close() }
        await client.refreshAgents(["initial", "unchanged", "changed", "observed", "unauthorized", "deleted", "history-error"], history: { id in
            id == "initial" ? .initial : id == "observed" ? .stateOnly : .changed(after: Cursor(rawValue: "1")!)
        }) { id, result in await received.append(id, result) }
        let values = await received.values
        XCTAssertEqual(values.count, 7)
        XCTAssertNotNil(values["initial"]?.result?.page)
        XCTAssertNotNil(values["changed"]?.result?.page)
        XCTAssertNil(values["unchanged"]?.result?.page)
        XCTAssertNil(values["observed"]?.result?.page)
        XCTAssertEqual(values["unauthorized"]?.error as? APIError, .http(401))
        XCTAssertEqual(values["deleted"]?.error as? APIError, .agentDeleting)
        XCTAssertEqual(values["history-error"]?.error as? APIError, .http(403))
        let history = fixture.queue.sync { requests.filter { $0.hasSuffix("/events/history") }.sorted() }
        XCTAssertEqual(history, ["changed", "history-error", "initial"].map { "/v1/agents/\($0)/events/history" })
    }

    func testHistoryOwnershipIsResolvedWhenAWaitingAgentStarts() async throws {
        let gate = DispatchGroup(); gate.enter()
        let four = expectation(description: "First four operations started"); four.expectedFulfillmentCount = 4
        let policy = RefreshPolicy()
        let received = RefreshResults()
        var paths: [String] = []
        let fixture = try HTTPFixture { request in
            paths.append(request.path)
            let id = String(request.path.split(separator: "/")[2])
            if id.hasPrefix("first-") { four.fulfill(); return FixtureReply(body: refreshState(id), gate: gate) }
            if request.path.hasSuffix("/events/history") { return FixtureReply(body: #"{"data":[],"has_more":false,"latest_cursor":"2"}"#) }
            return FixtureReply(body: refreshState(id, cursor: "2"))
        }
        defer { fixture.close() }
        let client = try refreshClient(fixture)
        defer { client.close() }
        let ids = (0..<4).map { "first-\($0)" } + ["previous-focus", "new-focus", "removed"]
        let task = Task {
            await client.refreshAgents(ids, history: { id in await policy.history(id) }) { id, result in
                await received.append(id, result)
            }
        }
        await fulfillment(of: [four], timeout: 5)
        await policy.switchFocus()
        gate.leave(); await task.value
        let values = await received.values
        XCTAssertNotNil(values["previous-focus"]?.result?.page, "A tab no longer observed needs history")
        XCTAssertNil(values["new-focus"]?.result?.page, "The new observer owns its history")
        XCTAssertNil(values["removed"], "An agent removed in another epoch must not publish a result")
        XCTAssertFalse(fixture.queue.sync { paths.contains("/v1/agents/removed") })
    }
}

private func refreshClient(_ fixture: HTTPFixture) throws -> ManagedClient {
    ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
}
private func refreshState(_ id: String, cursor: String = "0") -> String {
    "{\"agent_id\":\"\(id)\",\"latest_event_cursor\":\"\(cursor)\",\"active_turns\":[],\"settings\":{\"model\":\"gpt-6-astra\"}}"
}
private actor RefreshResults {
    struct Value { let result: AgentRefreshResult?; let error: Error?; let milliseconds: Double }
    let started: ContinuousClock.Instant
    var values: [String: Value] = [:]
    init(started: ContinuousClock.Instant = .now) { self.started = started }
    func append(_ id: String, _ result: Result<AgentRefreshResult, Error>) {
        let elapsed = started.duration(to: .now).components
        let milliseconds = Double(elapsed.seconds) * 1000 + Double(elapsed.attoseconds) / 1e15
        switch result {
        case .success(let value): values[id] = Value(result: value, error: nil, milliseconds: milliseconds)
        case .failure(let error): values[id] = Value(result: nil, error: error, milliseconds: milliseconds)
        }
    }
}
private actor RefreshPolicy {
    var switched = false
    func switchFocus() { switched = true }
    func history(_ id: String) -> AgentRefreshHistory? {
        if id == "removed" { return nil }
        if id == "previous-focus" { return switched ? .changed(after: .zero) : AgentRefreshHistory.stateOnly }
        if id == "new-focus" { return switched ? AgentRefreshHistory.stateOnly : .initial }
        return AgentRefreshHistory.stateOnly
    }
}
