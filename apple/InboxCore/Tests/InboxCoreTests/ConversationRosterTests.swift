import XCTest
@testable import InboxCore

final class ConversationRosterTests: XCTestCase {
    func testStreamingUpdatesKeepRowsUnderTheSameFinger() {
        var cards = [AgentCard(id: "older", title: "Older", updatedAt: 1),
                     AgentCard(id: "newer", title: "Newer", updatedAt: 2)]
        var roster = ConversationRoster(cards: cards)
        cards[0].updatedAt = 3
        cards[0].preview = "Fresh response"
        roster.reconcile(cards)
        XCTAssertEqual(roster.visible(in: cards, matching: "").map(\.id), ["newer", "older"])
        XCTAssertEqual(roster.visible(in: cards, matching: " response ").map(\.id), ["older"])
    }

    func testRemovedMembershipCannotRemainVisibleAndNewRowsAreUnique() {
        let first = AgentCard(id: "first", title: "First")
        let second = AgentCard(id: "second", title: "Second")
        var roster = ConversationRoster(cards: [first])
        roster.reconcile([second, second])
        XCTAssertEqual(roster.ids, ["second"])
        XCTAssertEqual(roster.visible(in: [second], matching: "").map(\.id), ["second"])
        XCTAssertEqual(roster.visible(in: [second], matching: "first"), [])
    }
}
