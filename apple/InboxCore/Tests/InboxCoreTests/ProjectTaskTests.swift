import XCTest
@testable import InboxCore

final class ProjectTaskTests: XCTestCase {
    func testCanonicalReferencesPreserveLocalNamesAndExcludeMainThread() {
        let cards = [AgentCard(id: "main", title: "Main Thread", updatedAt: 3),
                     AgentCard(id: "root", title: "Root", updatedAt: 2),
                     AgentCard(id: "other", title: "Other", updatedAt: 1)]
        let local = InboxProject(id: "local", name: "My local name", primaryAgentID: "root")
        let index = InboxProjectIndex(cards: cards, savedProjects: [local], canonicalProjects: [
            InboxProject(id: "canonical", name: "Shared name", primaryAgentID: "root"),
            InboxProject(id: "second", name: "Shared project", primaryAgentID: "other")
        ], mainThreadID: "main")
        XCTAssertEqual(index.projects.map(\.name), ["My local name", "Shared project"])
        XCTAssertEqual(index.projects.first?.id, "local")
        XCTAssertFalse(index.projects.contains { $0.agentIDs.contains("main") })
        XCTAssertEqual(local.name, "My local name")
    }

    func testMainCardAndDelegationLinksSurviveProjectExclusion() {
        var main = AgentCard(id: "main", title: "Main")
        main.projectRootID = "root"; main.parentAgentID = "root"; main.originTurnID = "turn"
        var child = AgentCard(id: "child", title: "Child")
        child.projectRootID = "main"; child.parentAgentID = "main"; child.originTurnID = "delegation"
        let saved = [InboxProject(id: "local", name: "Local", primaryAgentID: "root", agentIDs: ["root", "legacy"])]
        let index = InboxProjectIndex(cards: [main, child, AgentCard(id: "root", title: "Root")], savedProjects: saved,
            canonicalProjects: [InboxProject(id: "main-project", name: "Main", primaryAgentID: "main"),
                                InboxProject(id: "canonical", name: "Canonical", primaryAgentID: "root")], mainThreadID: "main")
        XCTAssertEqual(index.projects.map(\.id), ["local"])
        XCTAssertEqual(index.projects.first?.agentIDs, ["root"])
        XCTAssertEqual(index.cardsByID["main"], main)
        XCTAssertEqual(index.children(parentAgentID: "root", originTurnID: "turn"), [main])
        XCTAssertEqual(index.children(parentAgentID: "main", originTurnID: "delegation"), [child])
        XCTAssertEqual(saved[0].agentIDs, ["root", "legacy"])
    }

    func testCanonicalProjectIncludesOnlyServerDeclaredMembers() {
        let root = AgentCard(id: "root", title: "Root")
        var child = AgentCard(id: "child", title: "Child")
        child.projectRootID = "root"
        let unrelated = AgentCard(id: "unrelated", title: "Unrelated")
        let canonical = InboxProject(id: "shared", name: "Shared", primaryAgentID: "root")
        let index = InboxProjectIndex(cards: [root, child, unrelated], savedProjects: [], canonicalProjects: [canonical])
        XCTAssertEqual(index.projects.first?.id, "shared")
        XCTAssertEqual(index.projects.first?.agentIDs, ["root", "child"])
        XCTAssertEqual(index.projects.last?.primaryAgentID, "unrelated")
    }

    func testRosterPreservesServerProjectLineageAndTaskIdentity() async throws {
        let fixture = try HTTPFixture { request in
            XCTAssertEqual(request.path, "/v1/agents")
            return FixtureReply(body: #"{"data":["master","child"],"summaries":{"master":{"title":"Orbit"},"child":{"title":"Long prompt","project_title":"Fix sign-in","project_root_id":"master","parent_agent_id":"master","origin_turn_id":"request-1","project_turn_id":"project:fix"}}}"#)
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        let cards = try await client.list()
        XCTAssertNil(cards[0].projectRootID)
        XCTAssertEqual(cards[1].title, "Fix sign-in")
        XCTAssertEqual(cards[1].projectRootID, "master")
        XCTAssertEqual(cards[1].parentAgentID, "master")
        XCTAssertEqual(cards[1].originTurnID, "request-1")
        XCTAssertEqual(cards[1].projectTurnID, "project:fix")
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
