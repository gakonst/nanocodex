import XCTest
@testable import InboxCore

final class FocusedCardCacheTests: XCTestCase {
    func testComposerReadsReuseLookupUntilRosterOrSelectionChanges() {
        var cache = FocusedCardCache()
        var cards = (0..<10_000).map { AgentCard(id: "agent-\($0)", title: "Conversation \($0)") }
        for _ in 0..<100 {
            XCTAssertEqual(cache.card(id: "agent-9999", in: cards)?.title, "Conversation 9999")
        }
        XCTAssertEqual(cache.lookupCount, 1)
        cards[9999].activeTurns = ["new-turn"]
        cards[9999].title = "Updated title"
        cache.invalidate()
        XCTAssertEqual(cache.card(id: "agent-9999", in: cards)?.activeTurns, ["new-turn"])
        XCTAssertEqual(cache.card(id: "agent-9999", in: cards)?.title, "Updated title")
        XCTAssertEqual(cache.lookupCount, 2)
        XCTAssertEqual(cache.card(id: "agent-0", in: cards)?.id, "agent-0")
        XCTAssertEqual(cache.lookupCount, 3)
        cards.removeAll()
        cache.invalidate()
        XCTAssertNil(cache.card(id: "agent-0", in: cards))
        XCTAssertEqual(cache.lookupCount, 4)
    }

    func testMissingSelectionIsCachedAndNewlyCreatedCardAppearsAfterInvalidation() {
        var cache = FocusedCardCache()
        for _ in 0..<100 { XCTAssertNil(cache.card(id: "new", in: [])) }
        XCTAssertEqual(cache.lookupCount, 1)
        cache.invalidate()
        XCTAssertEqual(cache.card(id: "new", in: [.init(id: "new", title: "New")])?.title, "New")
        XCTAssertNil(cache.card(id: nil, in: [.init(id: "new", title: "New")]))
        XCTAssertEqual(cache.lookupCount, 3)
    }
}
