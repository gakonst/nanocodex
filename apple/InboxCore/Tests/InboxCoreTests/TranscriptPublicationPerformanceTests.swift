import XCTest
@testable import InboxCore

final class TranscriptPublicationPerformanceTests: XCTestCase {
    @MainActor
    func testLongHistoryPublicationPreparation() async throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_PUBLICATION_BENCHMARK"] == "1" else {
            throw XCTSkip("Opt in with NANOCODEX_PUBLICATION_BENCHMARK=1; use swift test -c release")
        }
        let events = try (1...8_000).map { index in
            try AgentEvent(.object([
                "cursor": .string(String(index)), "type": .string("turn_completed"),
                "turn_id": .string("synthetic-turn-\(index)"),
                "final_message": .string("Synthetic reply \(index) " + String(repeating: "retained transcript ", count: 64))
            ]))
        }
        let projected = transcript(events)
        var previous = projected
        previous[previous.count - 1].text += " previous"
        let previousRows = previous
        var initial = AgentCard(id: "synthetic-agent", title: "Benchmark")
        // Model an already-open long transcript receiving one additional event,
        // rather than repeatedly applying the entire window to an empty card.
        initial.apply(events: Array(events.dropLast()), transcriptRows: Array(projected.dropLast()))
        // Stabilize observedAt so synchronous and detached results are exactly comparable.
        initial.latestCursor = events.last!.cursor
        let base = initial
        let revision = UUID()
        let iterations = 40
        var oldMain: [Double] = [], newMain: [Double] = [], worker: [Double] = []
        func seconds(_ duration: Duration) -> Double {
            Double(duration.components.seconds) + Double(duration.components.attoseconds) / 1e18
        }
        for _ in 0..<iterations {
            let oldStart = ContinuousClock.now
            var expected = base
            expected.apply(events: events, transcriptRows: projected)
            let expectedChanged = previousRows != projected
            oldMain.append(seconds(oldStart.duration(to: .now)))

            let launchStart = ContinuousClock.now
            let task = Task.detached(priority: .userInitiated) {
                let start = ContinuousClock.now
                let prepared = TranscriptPublicationPreparation(events: events, rows: projected,
                    previousRows: previousRows, card: base, rowsRevision: revision)
                return (prepared, start.duration(to: .now))
            }
            let launchTime = seconds(launchStart.duration(to: .now))
            let (prepared, elapsed) = await task.value
            let changed = prepared.rowsChanged
            let publicationStart = ContinuousClock.now
            // Mirrors the base-card guard and bounded prepared-card comparison.
            let stillCurrent = prepared.isCurrent(rowsRevision: revision, card: initial)
            let needsPublication = initial != prepared.card
            newMain.append(launchTime + seconds(publicationStart.duration(to: .now)))
            worker.append(seconds(elapsed))
            XCTAssertTrue(stillCurrent)
            XCTAssertTrue(needsPublication)
            XCTAssertTrue(changed)
            XCTAssertEqual(changed, expectedChanged)
            XCTAssertEqual(prepared.card, expected)
            XCTAssertEqual(prepared.card?.appliedHistoryCursor, events.last?.cursor)
        }
        func median(_ values: [Double]) -> Double { values.sorted()[values.count / 2] * 1000 }
        print("PUBLICATION_PREPARATION_PERF events=\(events.count) rows=\(projected.count) iterations=\(iterations) old_main_ms=\(median(oldMain)) new_main_ms=\(median(newMain)) worker_ms=\(median(worker))")
    }
}
