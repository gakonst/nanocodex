import XCTest
@testable import InboxCore

final class SidebarPresentationTests: XCTestCase {
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
