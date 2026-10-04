import XCTest
@testable import InboxCore

// Isolated recovery tests are necessary: a UI journey cannot deterministically
// interrupt migration or inject malformed persisted intents before app launch.
final class MobileOutboxStoreTests: XCTestCase {
    func testMalformedMigrationRetainsAllLegacyKeysAndCanRecover() throws {
        let suite = "outbox-tests-" + UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let store = try MobileOutboxStore(path: ":memory:")
        let message = PendingMessage(agentID: "agent", input: "retained", predecessor: "", id: UUID().uuidString)
        defaults.set(try JSONEncoder().encode([message]), forKey: "inbox.pending.account")
        defaults.set(Data("broken".utf8), forKey: "inbox.steering.account")
        defaults.set(["creation-id"], forKey: "inbox.creations.account")
        XCTAssertThrowsError(try store.restore(scope: "account", defaults: defaults))
        XCTAssertNotNil(defaults.data(forKey: "inbox.pending.account"))
        XCTAssertNotNil(defaults.data(forKey: "inbox.steering.account"))
        defaults.set(try JSONEncoder().encode([SteeringTransfer]()), forKey: "inbox.steering.account")
        let restored = try store.restore(scope: "account", defaults: defaults)
        XCTAssertEqual(restored.pending, [message])
        XCTAssertEqual(restored.pendingCreations, ["creation-id"])
        XCTAssertTrue(restored.asyncCodeModeCreations.isEmpty)
        XCTAssertNil(defaults.object(forKey: "inbox.pending.account"))
    }

    func testRestartPreservesAllIntentsAndNeverResurrectsLegacyStateAcrossAccounts() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let path = directory.appendingPathComponent("outbox.sqlite").path
        let suite = "outbox-tests-" + UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let message = PendingMessage(agentID: "agent", input: "retained", predecessor: "", id: UUID().uuidString)
        let cancellation = PendingTurnCancellation(agentID: "agent", turnID: message.id)
        let transfer = SteeringTransfer(agentID: "agent", sourceTurnID: message.id, targetTurnID: "target")
        let snapshot = MobileOutboxStore.Snapshot(pending: [message], cancellations: [cancellation], steeringTransfers: [transfer], pendingCreations: ["creation-id"], asyncCodeModeCreations: ["creation-id"])
        do {
            let store = try MobileOutboxStore(path: path)
            _ = try store.restore(scope: "a", defaults: defaults)
            _ = try store.restore(scope: "b", defaults: defaults)
            try store.save(snapshot, scope: "a")
        }
        let reopened = try MobileOutboxStore(path: path)
        XCTAssertEqual(try reopened.restore(scope: "a", defaults: defaults), snapshot)
        XCTAssertEqual(try reopened.restore(scope: "b", defaults: defaults), .init())
        try reopened.save(.init(), scope: "a")
        // Simulate death after SQLite commit but before UserDefaults cleanup.
        defaults.set(try JSONEncoder().encode([message]), forKey: "inbox.pending.a")
        XCTAssertEqual(try reopened.restore(scope: "a", defaults: defaults), .init())
        XCTAssertNil(defaults.object(forKey: "inbox.pending.a"))
    }
}
