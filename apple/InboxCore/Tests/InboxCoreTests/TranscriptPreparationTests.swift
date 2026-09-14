import XCTest
@testable import InboxCore

final class TranscriptPreparationTests: XCTestCase {
    private func history() throws -> [AgentEvent] {
        try (1...400).map { index in
            try AgentEvent(.object([
                "cursor": .string(String(index)), "type": .string("turn_completed"),
                "turn_id": .string("turn-\(index)"),
                "final_message": .string(String(repeating: "Greek Ελληνικά 👋 **reply**\n", count: 128))
            ]))
        }
    }

    @MainActor
    func testProjectionAndByteAccountingPreserveCanonicalHistory() async throws {
        let events = try history()
        let expected = transcript(events)
        let expectedBytes = try events.map { try JSONEncoder().encode($0.data).count }
        async let rows = TranscriptPreparation.rows(events)
        async let bytes = TranscriptPreparation.byteCounts(events)
        let actual = try await (rows, bytes)
        XCTAssertEqual(actual.0, expected)
        XCTAssertEqual(actual.1, expectedBytes)
    }

    @MainActor
    func testCancelledProjectionCannotPublishRows() async throws {
        let events = try history()
        let task = Task { try await TranscriptPreparation.rows(events) }
        task.cancel()
        do { _ = try await task.value; XCTFail("Cancelled projection returned rows") }
        catch { XCTAssertTrue(error is CancellationError) }
    }

    @MainActor
    func testPreferenceBackpressurePreservesOrderAndAccountScope() async throws {
        let suite = "nanocodex-preferences-test-" + UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let writer = InboxPreferencesWriter(suiteName: suite)
        let gate = DispatchSemaphore(value: 0)
        writer.enqueue { _ in _ = gate.wait(timeout: .now() + 2) }
        defer { gate.signal() }
        for index in 0..<100 {
            writer.enqueue { store in store.set(["text": "draft-\(index)"], forKey: "account-a") }
        }
        writer.enqueue { store in store.set(["text": "other account"], forKey: "account-b") }
        let start = ContinuousClock.now
        try await Task.sleep(for: .milliseconds(20))
        XCTAssertLessThan(start.duration(to: .now), .milliseconds(250), "Saving must not block the UI executor")
        gate.signal()
        await writer.flush()
        XCTAssertEqual(defaults.dictionary(forKey: "account-a") as? [String: String], ["text": "draft-99"])
        XCTAssertEqual(defaults.dictionary(forKey: "account-b") as? [String: String], ["text": "other account"])
    }
}
