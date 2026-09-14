import Foundation
import XCTest
@testable import InboxCore

final class TurnCancellationTests: XCTestCase {
    private func receipt(state: String = "cancelling", updated: Double = 42, turn: String = "target") -> JSON {
        .object(["turn_id": .string(turn), "state": .string(state), "updated_at": .number(updated), "attempt_count": .number(7), "retry_at": .number(1)])
    }

    func testUnchangedDurableCancellationBacksOffWithoutExpiringIntent() throws {
        var schedule = TurnCancellationPollSchedule()
        var intent = PendingTurnCancellation(agentID: "agent", turnID: "target")
        intent.acknowledged = true
        let current = receipt()
        let delays = (0..<9).map { _ in schedule.delay(after: current) }
        XCTAssertEqual(delays, [1, 2, 4, 8, 15, 15, 15, 15, 15].map { Duration.seconds($0) })
        for _ in 0..<1000 {
            XCTAssertEqual(schedule.delay(after: current), .seconds(15))
            XCTAssertFalse(intent.isTerminal(receipt: current), "Hours of cancelling cannot silently finish Stop")
        }
        let restored = try JSONDecoder().decode(PendingTurnCancellation.self, from: JSONEncoder().encode(intent))
        XCTAssertEqual(restored, intent)
        XCTAssertEqual(try restored.command.requestSpec().path, "/v1/agents/agent/turns/target/cancel")
        var resumed = TurnCancellationPollSchedule()
        XCTAssertEqual(resumed.delay(after: current), .seconds(1), "Resume starts fresh after its immediate exact-turn request")
    }

    func testServerProgressResetsCadenceAndRetryTimestampDoesNotPostponeForever() {
        var schedule = TurnCancellationPollSchedule()
        let current = receipt()
        for _ in 0..<8 { _ = schedule.delay(after: current) }
        XCTAssertEqual(schedule.delay(after: receipt(updated: 43)), .seconds(1))
        XCTAssertEqual(schedule.delay(after: receipt(updated: 43)), .seconds(2))
        XCTAssertEqual(schedule.delay(after: receipt(state: "accepted", updated: 43)), .seconds(1))
        var future = receipt().objectValue
        future["retry_at"] = .number(9_999_999_999_999)
        var futureSchedule = TurnCancellationPollSchedule()
        XCTAssertEqual(futureSchedule.delay(after: .object(future)), .seconds(1))
        for _ in 0..<8 { _ = futureSchedule.delay(after: .object(future)) }
        XCTAssertEqual(futureSchedule.delay(after: .object(future)), .seconds(15))
    }

    func testOnlyExactTerminalReceiptCompletesCancellation() {
        let intent = PendingTurnCancellation(agentID: "agent", turnID: "target")
        for state in ["completed", "cancelled", "failed"] {
            XCTAssertTrue(intent.isTerminal(receipt: receipt(state: state)))
            XCTAssertFalse(intent.isTerminal(receipt: receipt(state: state, turn: "other")))
        }
        for state in ["accepted", "cancelling", "", "unknown"] {
            XCTAssertFalse(intent.isTerminal(receipt: receipt(state: state)))
        }
        XCTAssertFalse(intent.isTerminal(receipt: .object(["active_turns": .array([])])))
    }

    @MainActor
    func testForegroundReplacementStartsImmediatelyAndOldCleanupCannotRemoveIt() async {
        let tasks = TurnCancellationTasks()
        let oldStarted = expectation(description: "Old request started")
        let oldFinished = expectation(description: "Cancelled old request eventually returns")
        let replacementStarted = expectation(description: "Foreground check starts immediately")
        let replacementStopped = expectation(description: "Terminal event cancels sleeping replacement")
        var releaseOld: CheckedContinuation<Void, Never>?
        tasks.start("target") {
            await withCheckedContinuation { releaseOld = $0; oldStarted.fulfill() }
            XCTAssertTrue(Task.isCancelled)
            oldFinished.fulfill()
        }
        await fulfillment(of: [oldStarted], timeout: 1)
        tasks.start("target") { XCTFail("Duplicate resume must not start another task") }
        tasks.start("target", restart: true) {
            replacementStarted.fulfill()
            do { try await Task.sleep(for: .seconds(15)); XCTFail("Finish must wake the sleep") }
            catch { XCTAssertTrue(Task.isCancelled) }
            replacementStopped.fulfill()
        }
        await fulfillment(of: [replacementStarted], timeout: 1)
        releaseOld?.resume()
        await fulfillment(of: [oldFinished], timeout: 1)
        XCTAssertTrue(tasks.contains("target"), "Old task cleanup cannot unregister its replacement")
        tasks.cancel("target")
        await fulfillment(of: [replacementStopped], timeout: 1)
        XCTAssertFalse(tasks.contains("target"))
    }

    @MainActor
    func testAccountResetCancelsAllWaitingTasks() async {
        let tasks = TurnCancellationTasks()
        let started = expectation(description: "Both targets started"); started.expectedFulfillmentCount = 2
        let stopped = expectation(description: "Both targets cancelled"); stopped.expectedFulfillmentCount = 2
        for id in ["a", "b"] {
            tasks.start(id) {
                started.fulfill()
                do { try await Task.sleep(for: .seconds(15)); XCTFail("Reset must wake the sleep") }
                catch { XCTAssertTrue(Task.isCancelled) }
                stopped.fulfill()
            }
        }
        await fulfillment(of: [started], timeout: 1)
        tasks.cancelAll()
        await fulfillment(of: [stopped], timeout: 1)
        XCTAssertFalse(tasks.contains("a")); XCTAssertFalse(tasks.contains("b"))
    }
}

private extension JSON {
    var objectValue: [String: JSON] { if case .object(let value) = self { return value }; return [:] }
}
