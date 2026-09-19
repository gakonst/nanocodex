import XCTest
@testable import InboxCore

final class InboxRosterProjectionTests: XCTestCase {
    func testSnapshotRetainsOpenFocusAndStableTabsAndFiltersClosed() async {
        let cards = [AgentCard(id: "a", title: "A", updatedAt: 2), AgentCard(id: "b", title: "B", updatedAt: 3), AgentCard(id: "closed", title: "Closed")]
        let snapshot = InboxRosterProjection(cards: cards, focusedID: "a", opened: ["b", "closed", "missing"], closed: ["closed"], tabOrder: ["b", "b", "missing"], pinnedID: "a", filter: .all, seen: [:], deferred: [:])
        let result = await Task.detached { snapshot.resolve() }.value
        XCTAssertEqual(result.opened, ["b", "closed"])
        XCTAssertEqual(result.tabs, ["b", "a"])
        XCTAssertEqual(result.eligible, ["b", "a"])
    }

    func testRunningFilterPreservesPinnedConversation() {
        var running = AgentCard(id: "running", title: "Running")
        running.activeTurns = ["turn"]
        let snapshot = InboxRosterProjection(cards: [running, .init(id: "pinned", title: "Pinned"), .init(id: "idle", title: "Idle")], focusedID: "pinned", opened: ["idle"], closed: [], tabOrder: [], pinnedID: "pinned", filter: .running, seen: [:], deferred: [:])
        XCTAssertEqual(Set(snapshot.resolve().eligible), ["running", "pinned"])
    }
}
