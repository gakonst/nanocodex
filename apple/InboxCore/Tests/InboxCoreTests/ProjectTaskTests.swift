import XCTest
@testable import InboxCore

final class ProjectTaskTests: XCTestCase {
    func testRosterPreservesServerProjectLineageAndTaskIdentity() async throws {
        let fixture = try HTTPFixture { request in
            XCTAssertEqual(request.path, "/v1/agents")
            return FixtureReply(body: #"{"data":["master","child"],"summaries":{"master":{"title":"Orbit"},"child":{"title":"Long prompt","project_title":"Fix sign-in","project_root_id":"master","parent_agent_id":"master","origin_turn_id":"request-1","project_turn_id":"project:fix"}}}"#)
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        let cards = try await client.list()
        XCTAssertNil(cards[0].projectName)
        XCTAssertNil(cards[0].projectRootID)
        XCTAssertEqual(cards[1].title, "Fix sign-in")
        XCTAssertEqual(cards[1].projectRootID, "master")
        XCTAssertEqual(cards[1].parentAgentID, "master")
        XCTAssertEqual(cards[1].originTurnID, "request-1")
        XCTAssertEqual(cards[1].projectTurnID, "project:fix")
    }

    func testCanonicalProjectsOverrideStaleLocalNamesWithoutChangingExecutionLinks() async throws {
        let fixture = try HTTPFixture { _ in
            FixtureReply(body: #"{"data":["alpha","beta","gamma","old","child"],"summaries":{"alpha":{"title":"Old title","project_root_id":"alpha","project_name":"Project Alpha"},"beta":{"title":"Beta","project_root_id":"beta","project_name":"Project Beta"},"gamma":{"title":"Gamma","project_root_id":"gamma","project_name":"Project Gamma"},"old":{"title":"Old project","project_root_id":"alpha","project_name":"Project Alpha"},"child":{"title":"Task","project_root_id":"alpha","project_name":"Project Alpha","parent_agent_id":"old","origin_turn_id":"request-1","project_turn_id":"project:fix"}}}"#)
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        let cards = try await client.list()
        let index = InboxProjectIndex(cards: cards, savedProjects: [
            InboxProject(id: "saved-alpha", name: "Stale Alpha", primaryAgentID: "alpha"),
            InboxProject(id: "saved-beta", name: "Stale Beta", primaryAgentID: "beta"),
            InboxProject(id: "saved-old", name: "Obsolete project", primaryAgentID: "old")
        ])
        XCTAssertEqual(index.projects.count, 3)
        XCTAssertEqual(Set(index.projects.map(\.name)), ["Project Alpha", "Project Beta", "Project Gamma"])
        XCTAssertEqual(index.projects.first?.id, "saved-alpha")
        XCTAssertEqual(index.projects.first?.agentIDs, ["alpha", "old", "child"])
        XCTAssertEqual(index.cardsByID["alpha"]?.projectName, "Project Alpha")
        XCTAssertEqual(index.cardsByID["child"]?.projectName, "Project Alpha")
        let children = index.children(parentAgentID: "old", originTurnID: "request-1")
        XCTAssertEqual(children.map(\.id), ["child"])
        XCTAssertEqual(children.first?.projectTurnID, "project:fix")
        XCTAssertTrue(index.children(parentAgentID: "alpha", originTurnID: "request-1").isEmpty)
    }

    func testTaskIdentityAndTerminalOutcomeDoNotDependOnPartialHistory() throws {
        var user = TranscriptRow(id: "input", role: "You", text: "Fix sign in")
        user.turnID = "one"
        let completed = try AgentEvent(.object(["type": .string("turn_completed"), "turn_id": .string("one")]), cursor: "10")
        let tasks = ProjectTask.project(agentID: "main", rows: [user], events: [completed], activeTurns: [], pending: [])
        XCTAssertEqual(tasks.first?.id, "main:one")
        XCTAssertEqual(tasks.first?.title, "Fix sign in")
        XCTAssertEqual(tasks.first?.status, "Completed")
        XCTAssertFalse(tasks.first!.isLive)
    }
    func testActiveAndQueuedTasksWithoutLoadedInputsStayVisible() {
        let tasks = ProjectTask.project(agentID: "main", rows: [], events: [], activeTurns: ["one", "two"], pending: [])
        XCTAssertEqual(tasks.count, 2)
        XCTAssertEqual(tasks.first { $0.turnID == "one" }?.status, "Working")
        XCTAssertEqual(tasks.first { $0.turnID == "two" }?.status, "Queued")
        XCTAssertTrue(tasks.allSatisfy(\.isLive))
    }
    func testPartialHistoryDoesNotInventCompletion() {
        var row = TranscriptRow(id: "partial", role: "Agent", text: "Checking")
        row.turnID = "one"; row.phase = "commentary"
        let tasks = ProjectTask.project(agentID: "main", rows: [row], events: [], activeTurns: [], pending: [])
        XCTAssertEqual(tasks.first?.status, "History")
    }
    func testProjectCreationIdentityMigrationAndRoundTrip() throws {
        var project = InboxProject(id: "project", name: "Orbit", primaryAgentID: "draft", agentIDs: ["draft", "server", "other"])
        project.replaceAgent("draft", with: "server")
        XCTAssertEqual(project.primaryAgentID, "server")
        XCTAssertEqual(project.agentIDs, ["server", "other"])
        XCTAssertEqual(try JSONDecoder().decode(InboxProject.self, from: JSONEncoder().encode(project)), project)
    }
    func testProjectIndexPreservesMembershipOrphansSavedNamesAndLinkOrder() {
        let root = AgentCard(id: "root", title: "Root", updatedAt: 1)
        var child = AgentCard(id: "child", title: "Child", updatedAt: 3)
        child.projectRootID = "root"; child.parentAgentID = "root"; child.originTurnID = "request"
        var nested = AgentCard(id: "nested", title: "Nested", updatedAt: 4)
        nested.projectRootID = "root"; nested.parentAgentID = "child"; nested.originTurnID = "request"
        var orphan = AgentCard(id: "orphan", title: "Orphan", updatedAt: 5)
        orphan.projectRootID = "missing"
        let cards = [root, child, nested, orphan]
        let index = InboxProjectIndex(cards: cards, savedProjects: [
            InboxProject(id: "saved", name: "Renamed", primaryAgentID: "root"),
            InboxProject(id: "deleted", name: "Deleted", primaryAgentID: "gone")
        ])
        XCTAssertEqual(index.projects.map(\.name), ["Renamed", "Orphan"])
        XCTAssertEqual(index.projects.first?.agentIDs, ["root", "child", "nested"])
        XCTAssertEqual(index.children(parentAgentID: "root", originTurnID: "request").map(\.id), ["child"])
        XCTAssertEqual(index.children(parentAgentID: "child", originTurnID: "request").map(\.id), ["nested"])
        XCTAssertTrue(index.children(parentAgentID: "root", originTurnID: "other").isEmpty)
        XCTAssertEqual(index.cardsByID["nested"], nested)
    }

    func testLargeProjectIndexKeepsEveryChildAndRebuildsChangedLinks() {
        let root = AgentCard(id: "root", title: "Root")
        var children = (0..<40).map { index -> AgentCard in
            var card = AgentCard(id: "child-\(index)", title: "Child")
            card.projectRootID = "root"; card.parentAgentID = "root"; card.originTurnID = "request"
            return card
        }
        let first = InboxProjectIndex(cards: [root] + children, savedProjects: [])
        XCTAssertEqual(first.projects.first?.agentIDs.count, 41)
        XCTAssertEqual(first.children(parentAgentID: "root", originTurnID: "request").count, 40)
        children[0].originTurnID = "new-request"
        let updated = InboxProjectIndex(cards: [root] + children, savedProjects: [])
        XCTAssertEqual(updated.children(parentAgentID: "root", originTurnID: "request").count, 39)
        XCTAssertEqual(updated.children(parentAgentID: "root", originTurnID: "new-request").map(\.id), ["child-0"])
        XCTAssertEqual(first.children(parentAgentID: "root", originTurnID: "request").count, 40)
    }

    func testSummaryMatchesFullProjectionWithRepeatedRowsAndTerminalPrecedence() throws {
        var first = TranscriptRow(id: "first", role: "You", text: "First")
        first.turnID = "one"
        var second = TranscriptRow(id: "second", role: "Agent", text: "Done")
        second.turnID = "two"; second.phase = "final"
        var repeated = TranscriptRow(id: "repeat", role: "Agent", text: "Earlier turn")
        repeated.turnID = "one"
        let rows = [first, second, repeated]
        let events = try ["turn_completed", "turn_failed", "turn_cancelled"].enumerated().map { index, type in
            try AgentEvent(.object(["type": .string(type), "turn_id": .string("one")]), cursor: "\(index + 1)")
        }
        for active in [[], ["one"], ["three", "four"], ["", "two"]] {
            for history in [[], events] {
                let full = ProjectTask.project(agentID: "child", rows: rows, events: history, activeTurns: active, pending: [])
                let expected = active.first.flatMap { id in full.first { $0.turnID == id } } ?? full.first
                let summary = ProjectTask.summary(agentID: "child", rows: rows, events: history, activeTurns: active, pending: [])
                XCTAssertEqual(summary?.id, expected?.id)
                XCTAssertEqual(summary?.status, expected?.status)
                XCTAssertEqual(summary?.isLive, expected?.isLive)
                XCTAssertEqual(summary?.rows.count, 0)
            }
        }
        XCTAssertNil(ProjectTask.summary(agentID: "child", rows: [], events: [], activeTurns: [], pending: []))
    }

    func testSummaryMatchesPendingOrderFailureAndOtherAgentIsolation() {
        var row = TranscriptRow(id: "old", role: "You", text: "Loaded")
        row.turnID = "loaded"
        let queued = PendingMessage(agentID: "child", input: "Queued", predecessor: "", id: "queued")
        var failed = PendingMessage(agentID: "child", input: "Retry", predecessor: "", id: "failed")
        failed.phase = .failed
        let other = PendingMessage(agentID: "other", input: "Unrelated", predecessor: "", id: "other")
        let duplicate = PendingMessage(agentID: "child", input: "Loaded", predecessor: "", id: "loaded")
        for pending in [[queued], [queued, failed, other], [queued, duplicate], [other]] {
            for active in [[], ["queued"]] {
                let full = ProjectTask.project(agentID: "child", rows: [row], events: [], activeTurns: active, pending: pending)
                let expected = active.first.flatMap { id in full.first { $0.turnID == id } } ?? full.first
                let summary = ProjectTask.summary(agentID: "child", rows: [row], events: [], activeTurns: active, pending: pending)
                XCTAssertEqual(summary?.id, expected?.id)
                XCTAssertEqual(summary?.status, expected?.status)
                XCTAssertTrue(summary?.rows.isEmpty == true)
            }
        }
    }

    func testFortyChildSummaryCacheReusesDraftReadsAndInvalidatesOnlyChangedChild() {
        var cache = ProjectTaskSummaryCache()
        var computations: [String: Int] = [:]
        let rows = (0..<500).map { index -> TranscriptRow in
            var row = TranscriptRow(id: "row-\(index)", role: "You", text: "Loaded history")
            row.turnID = "turn-\(index)"
            return row
        }
        func read(_ id: String, active: [String] = [], pending: [PendingMessage] = []) -> ProjectTask {
            cache.value(agentID: id, activeTurns: active, pending: pending) {
                computations[id, default: 0] += 1
                return ProjectTask.summary(agentID: id, rows: rows, events: [], activeTurns: active, pending: pending)!
            }
        }
        for _ in 0..<10 {
            for index in 0..<40 {
                let summary = read("child-\(index)")
                XCTAssertEqual(summary.turnID, "turn-499")
                XCTAssertTrue(summary.rows.isEmpty)
            }
        }
        XCTAssertEqual(computations.count, 40)
        XCTAssertTrue(computations.values.allSatisfy { $0 == 1 }, "Draft-like reads must reuse every child, including children beyond the old 16-entry limit")
        cache.invalidate(agentID: "child-17")
        for index in 0..<40 { _ = read("child-\(index)") }
        XCTAssertEqual(computations["child-17"], 2)
        XCTAssertEqual(computations.values.reduce(0, +), 41, "One child's history revision must not recompute other children")
        XCTAssertEqual(read("child-17", active: ["new-turn"]).status, "Working")
        XCTAssertEqual(computations["child-17"], 3)
        let pending = [PendingMessage(agentID: "child-18", input: "Queued", predecessor: "", id: "pending")]
        XCTAssertEqual(read("child-18", pending: pending).status, "Sending")
        XCTAssertEqual(computations["child-18"], 2)
        cache.removeAll()
        _ = read("child-0")
        XCTAssertEqual(computations["child-0"], 2, "Account/history reset must discard summaries")
    }

    func testRunningSummaryKeepsProjectStatusOverrideAndActiveIdentity() throws {
        var old = TranscriptRow(id: "old", role: "Agent", text: "Prior history")
        old.turnID = "old"; old.phase = "final"
        let completed = try AgentEvent(.object(["type": .string("turn_completed"), "turn_id": .string("active")]), cursor: "1")
        let summary = ProjectTask.summary(agentID: "child", rows: [old], events: [completed],
                                          activeTurns: ["active", "queued"], pending: [], isRunning: true)
        XCTAssertEqual(summary?.turnID, "active")
        XCTAssertEqual(summary?.status, "Working", "Match projectTasks' existing running-card override even with a terminal event")
        XCTAssertTrue(summary?.rows.isEmpty == true)
        let historical = ProjectTask.summary(agentID: "child", rows: [old], events: [completed],
                                             activeTurns: ["active"], pending: [])
        XCTAssertEqual(historical?.status, "Completed", "Non-running summaries must retain terminal precedence")
        let noActive = ProjectTask.summary(agentID: "child", rows: [old], events: [], activeTurns: [], pending: [], isRunning: true)
        XCTAssertEqual(noActive?.turnID, "old", "A running card without an active ID still uses history selection")
    }

}
