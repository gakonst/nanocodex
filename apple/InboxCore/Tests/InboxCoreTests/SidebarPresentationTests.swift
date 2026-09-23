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
    func testLatestPromptIsOptionalVersionedAndDoesNotRenameManualTitle() {
        var card = AgentCard(id: "agent", title: "My manual name")
        card.applyPresentation(.object([
            "status": .string("running"), "updatedAt": .number(200),
            "lastUserPrompt": .string("Check the mobile navigation")
        ]))
        XCTAssertEqual(card.sidebarLastUserPrompt, "Check the mobile navigation")
        XCTAssertEqual(card.title, "My manual name")
        XCTAssertTrue(card.isRunningInSidebar)
        card.applyPresentation(.object([
            "status": .string("idle"), "updatedAt": .number(100),
            "lastUserPrompt": .string("Older prompt")
        ]))
        XCTAssertEqual(card.sidebarLastUserPrompt, "Check the mobile navigation")
        XCTAssertTrue(card.isRunningInSidebar)
        card.applyPresentation(.object(["status": .string("completed"), "updatedAt": .number(300)]))
        XCTAssertEqual(card.sidebarLastUserPrompt, "")
        XCTAssertFalse(card.isRunningInSidebar)
    }

    func testLocalPromptWinsUntilNewerPresentationArrives() {
        var card = AgentCard(id: "agent", title: "Manual title")
        card.applyPresentation(.object(["status": .string("running"), "updatedAt": .number(100), "lastUserMessageAt": .number(100), "lastUserPrompt": .string("Old prompt")]))
        card.noteSubmittedPrompt("New local prompt", at: 200)
        XCTAssertEqual(card.sidebarLastUserPrompt, "New local prompt")
        card.applyPresentation(.object(["status": .string("running"), "updatedAt": .number(250), "lastUserMessageAt": .number(100), "lastUserPrompt": .string("Old prompt")]))
        XCTAssertEqual(card.sidebarLastUserPrompt, "New local prompt")
        XCTAssertEqual(card.lastUserMessageAt, 200)
        card.applyPresentation(.object(["status": .string("running"), "updatedAt": .number(300), "lastUserMessageAt": .number(300), "lastUserPrompt": .string("New remote prompt")]))
        XCTAssertEqual(card.sidebarLastUserPrompt, "New remote prompt")
        XCTAssertEqual(card.title, "Manual title")
    }

    func testNewRosterActivityWinsOverPreviouslyCheckedTurn() {
        var card = AgentCard(id: "agent", title: "Agent")
        card.checked = true; card.activeTurns = ["previous"]
        card.applyPresentation(.object([
            "status": .string("stopping"), "updatedAt": .number(200),
            "activeTurnIds": .array([.string("current")]), "activityTurnId": .string("current"),
            "activity": .string("Finishing the current operation")
        ]))
        XCTAssertTrue(card.isRunningInSidebar)
        XCTAssertEqual(card.sidebarActivity, "Finishing the current operation")
        card.applyPresentation(.object([
            "status": .string("completed"), "updatedAt": .number(300), "activeTurnIds": .array([])
        ]))
        XCTAssertFalse(card.isRunningInSidebar)
        XCTAssertEqual(card.sidebarActivity, "")
    }

}
