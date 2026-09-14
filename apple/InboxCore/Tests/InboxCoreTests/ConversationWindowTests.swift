import XCTest
@testable import InboxCore

final class ConversationWindowTests: XCTestCase {
    let now = Date(timeIntervalSince1970: 200_000)
    func testWindowBoundaryAndExplicitExceptions() {
        let boundary = (now.timeIntervalSince1970 - ConversationWindow.duration) * 1000
        let recent = AgentCard(id: "recent", title: "", updatedAt: boundary)
        var old = AgentCard(id: "old", title: "", updatedAt: boundary - 1)
        XCTAssertTrue(ConversationWindow.includes(recent, now: now))
        XCTAssertFalse(ConversationWindow.includes(old, now: now))
        XCTAssertTrue(ConversationWindow.includes(old, focusedID: "old", now: now))
        XCTAssertTrue(ConversationWindow.includes(old, openedIDs: ["old"], now: now))
        old.activeTurns = ["running"]
        XCTAssertTrue(ConversationWindow.includes(old, now: now))
    }
    func testOlderOverviewIsLazyStableAndDoesNotOpenTabs() {
        let cards = (0..<50).map { AgentCard(id: String($0), title: "", updatedAt: Double($0)) }
        XCTAssertTrue(ConversationWindow.overview(cards, now: now).isEmpty)
        let page = ConversationWindow.overview(cards, olderLimit: 24, now: now)
        XCTAssertEqual(page.count, 24)
        XCTAssertEqual(page.first?.id, "49")
        XCTAssertFalse(ConversationWindow.includes(page[0], now: now))
        XCTAssertEqual(ConversationWindow.overview(cards, olderLimit: 48, now: now).count, 48)
        XCTAssertEqual(ConversationWindow.overview(cards, openedIDs: ["0"], olderLimit: 24, now: now).count, 25)
    }
}
