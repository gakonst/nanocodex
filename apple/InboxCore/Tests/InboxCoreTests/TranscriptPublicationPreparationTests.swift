import XCTest
@testable import InboxCore

final class TranscriptPublicationPreparationTests: XCTestCase {
    @MainActor
    func testAsyncPreparationRejectsConcurrentRefreshAndNavigation() async throws {
        let revision = UUID()
        let base = AgentCard(id: "synthetic-agent", title: "Before refresh")
        let event = try AgentEvent(.object([
            "cursor": .string("1"), "type": .string("turn_completed"),
            "turn_id": .string("synthetic-turn"), "final_message": .string("Complete reply")
        ]))
        let rows = transcript([event])
        let task = Task.detached {
            TranscriptPublicationPreparation(events: [event], rows: rows,
                previousRows: [], card: base, rowsRevision: revision)
        }
        // These state changes happen before the UI actor can publish the result.
        var refreshed = base
        refreshed.activeTurns = ["newer-turn"]
        let navigationRevision = UUID()
        let prepared = await task.value
        XCTAssertTrue(prepared.isCurrent(rowsRevision: revision, card: base))
        XCTAssertFalse(prepared.isCurrent(rowsRevision: revision, card: refreshed))
        XCTAssertFalse(prepared.isCurrent(rowsRevision: navigationRevision, card: base))
        XCTAssertFalse(prepared.isCurrent(rowsRevision: revision, card: nil))
        XCTAssertTrue(prepared.rowsChanged)
        XCTAssertEqual(prepared.card?.preview, "Complete reply")
        XCTAssertEqual(prepared.card?.appliedHistoryCursor, event.cursor)
    }

    func testUnchangedRowsDoNotRequireMediaOrTranscriptPublication() throws {
        let revision = UUID()
        let rows = [TranscriptRow(id: "synthetic-row", role: "Agent", text: "Retained reply")]
        let prepared = TranscriptPublicationPreparation(events: [], rows: rows,
            previousRows: rows, card: nil, rowsRevision: revision)
        XCTAssertFalse(prepared.rowsChanged)
        XCTAssertTrue(prepared.isCurrent(rowsRevision: revision, card: nil))
    }
}
