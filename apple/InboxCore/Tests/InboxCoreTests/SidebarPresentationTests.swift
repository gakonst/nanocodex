import XCTest
@testable import InboxCore

final class SidebarPresentationTests: XCTestCase {
    func testSidebarOrdersByLastUserMessageInsteadOfAgentOutput() throws {
        var earlier = AgentCard(id: "earlier", title: "Earlier", updatedAt: 10, lastUserMessageAt: 10)
        let later = AgentCard(id: "later", title: "Later", updatedAt: 20, lastUserMessageAt: 20)
        let empty = AgentCard(id: "empty", title: "Empty", updatedAt: 999, lastUserMessageAt: 0)
        earlier.updatedAt = 1000 // A late reply must not change sidebar ordering.
        XCTAssertEqual([earlier, empty, later].sorted(by: AgentCard.mostRecentlyMessagedFirst).map(\.id), ["later", "earlier", "empty"])
        earlier.lastUserMessageAt = 30
        XCTAssertEqual([later, earlier].sorted(by: AgentCard.mostRecentlyMessagedFirst).map(\.id), ["earlier", "later"])
    }

    func testRosterStatusAndCurrentWork() {
        var card = AgentCard(id: "agent", title: "Fix sidebar")
        XCTAssertEqual(card.sidebarStatus, "Status unavailable")
        card.applyPresentation(.object([
            "status": .string("running"), "updatedAt": .number(100),
            "activeTurnIds": .array([.string("turn")]), "activityTurnId": .string("turn"),
            "activity": .string("I'm checking sidebar state")
        ]))
        XCTAssertEqual(card.sidebarStatus, "Running")
        XCTAssertEqual(card.sidebarActivity, "I'm checking sidebar state")
        card.applyPresentation(.object([
            "status": .string("completed"), "updatedAt": .number(200), "activeTurnIds": .array([])
        ]))
        XCTAssertEqual(card.sidebarStatus, "Ready")
        XCTAssertEqual(card.sidebarActivity, "")
        card.applyPresentation(.object(["status": .string("running"), "updatedAt": .number(100)]))
        XCTAssertEqual(card.sidebarStatus, "Ready")
    }
    func testOlderTurnCannotSupplyCurrentActivity() {
        var card = AgentCard(id: "agent", title: "Fix sidebar")
        card.checked = true; card.status = "Running"; card.activeTurns = ["new"]
        card.presentationActivity = "I'm checking old work"; card.presentationTurnID = "old"
        XCTAssertEqual(card.sidebarActivity, "Working")
    }
}
