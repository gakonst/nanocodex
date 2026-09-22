import Foundation
import XCTest
@testable import InboxCore

final class AgentThreadNotificationTests: XCTestCase {
    private func thread(_ id: String, turn: String = "run", status: String = "Running", text: String = "Working") -> AgentThreadNotification {
        .init(id: id, revision: status + ":" + turn, title: id, subtitle: status, body: text, isRunning: status == "Running")
    }
    func testRunningProgressAndHistoricalOutcomesStaySilent() {
        var ledger = AgentNotificationLedger()
        let threads = (0..<100).map { thread("agent-\($0)") }
        let old = thread("old", status: "Ready")
        XCTAssertEqual(ledger.reconcile(threads + [old], retaining: []), [])
        XCTAssertEqual(threads.filter { ledger.shouldPublish($0, foreground: false) }.count, 0)
        XCTAssertFalse(ledger.shouldPublish(old, foreground: false))
        XCTAssertFalse(ledger.shouldPublish(threads[0], foreground: true))
        ledger.didPublish(threads[0])
        XCTAssertFalse(ledger.shouldPublish(threads[0], foreground: false))
        XCTAssertFalse(ledger.shouldPublish(threads[1], foreground: false))
        let ready = thread("agent-1", status: "Ready")
        XCTAssertTrue(ledger.shouldPublish(ready, foreground: false))
        XCTAssertFalse(ledger.shouldPublish(ready, foreground: true))
        ledger.didPublish(ready)
        XCTAssertFalse(ledger.shouldPublish(thread("agent-1", status: "Ready", text: "Updated excerpt"), foreground: false))
        XCTAssertTrue(ledger.shouldPublish(thread("agent-1", turn: "next", status: "Ready"), foreground: false))
    }
    func testDismissalSurvivesRelaunchAndProgressButNewWorkCanNotify() throws {
        var ledger = AgentNotificationLedger()
        let a = thread("a"), b = thread("b")
        _ = ledger.reconcile([a, b], retaining: [])
        ledger.didPublish(a); ledger.didPublish(b); ledger.dismiss(id: "a", revision: a.revision)
        ledger = try JSONDecoder().decode(AgentNotificationLedger.self, from: JSONEncoder().encode(ledger))
        XCTAssertFalse(ledger.shouldPublish(thread("a", text: "New progress"), foreground: false))
        XCTAssertFalse(ledger.shouldPublish(thread("b", text: "New progress"), foreground: false))
        XCTAssertTrue(ledger.shouldPublish(thread("a", status: "Ready"), foreground: false))
        XCTAssertFalse(ledger.shouldPublish(thread("a", turn: "next-run"), foreground: false))
        XCTAssertFalse(String(decoding: try JSONEncoder().encode(ledger), as: UTF8.self).contains("Working"))
    }
    func testOutcomeReceiptSurvivesRelaunchAndFailureTextChanges() throws {
        var ledger = AgentNotificationLedger()
        _ = ledger.reconcile([thread("a")], retaining: [])
        let failed = thread("a", status: "Failed")
        XCTAssertTrue(ledger.shouldPublish(failed, foreground: false))
        ledger.didPublish(failed)
        ledger = try JSONDecoder().decode(AgentNotificationLedger.self, from: JSONEncoder().encode(ledger))
        XCTAssertFalse(ledger.shouldPublish(thread("a", status: "Failed", text: "More detail"), foreground: false))
        XCTAssertTrue(ledger.shouldPublish(thread("a", turn: "retry", status: "Failed"), foreground: false))
    }
    func testUncheckedRestorationRetainsReceiptsAndVerifiedRemovalCleansOnlyThatThread() {
        var ledger = AgentNotificationLedger()
        let a = thread("a"), b = thread("b")
        _ = ledger.reconcile([a, b], retaining: [])
        ledger.didPublish(a); ledger.didPublish(b)
        XCTAssertEqual(ledger.reconcile([], retaining: ["a", "b"]), [])
        XCTAssertEqual(ledger.reconcile([b], retaining: []), ["a"])
        XCTAssertNil(ledger.published["a"])
        XCTAssertNotNil(ledger.published["b"])
    }
    func testLateDismissalCannotHideNewTurn() {
        var ledger = AgentNotificationLedger()
        let old = thread("a"), new = thread("a", turn: "new")
        _ = ledger.reconcile([old], retaining: [])
        ledger.didPublish(old); ledger.didPublish(new); ledger.dismiss(id: "a", revision: old.revision)
        XCTAssertNil(ledger.dismissed["a"])
    }
    func testThreadProjectionKeepsIdentitiesExcerptsAndQueueSeparate() throws {
        var a = AgentCard(id: "a", title: "Fix reconnect")
        a.activeTurns = ["run-a"]; a.checked = true
        var b = AgentCard(id: "b", title: "Review docs")
        b.activeTurns = ["run-b"]; b.checked = true
        var queued = PendingMessage(agentID: "a", input: "Private input", predecessor: "run-a")
        queued.phase = .queued
        let threads = AgentThreadNotification.make(cards: [a, b], seen: [:], deferred: [:], pending: [queued])
        XCTAssertEqual(Set(threads.map(\.id)), ["a", "b"])
        XCTAssertTrue(threads.first(where: { $0.id == "a" })!.body.contains("1 queued follow-up."))
        XCTAssertFalse(threads.first(where: { $0.id == "b" })!.body.contains("queued"))
        XCTAssertTrue(threads.allSatisfy { !$0.body.contains("Private input") && $0.subtitle == "Running when last checked" })
        XCTAssertNotEqual(threads[0].fingerprint, threads[1].fingerprint)
    }
}
