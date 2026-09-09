import XCTest
@testable import InboxCore

final class TranscriptRetentionTests: XCTestCase {
    func testSixHundredLiveChunksRetainTheBeginningThroughFinalization() throws {
        // Exercise both the tab overview and focused conversation budgets.
        for budget in [8 * 1024 * 1024, 16 * 1024 * 1024] {
            var events: [AgentEvent] = [], bytes: [Int] = [], total = 0
            for index in 1...600 {
                let event = try AgentEvent(.object([
                    "cursor": .string(String(index)), "turn_id": .string("turn"), "type": .string("event"),
                    "event": .object(["type": .string("assistant.delta"), "payload": .object([
                        "text": .string(index == 1 ? "Beginning " : "chunk "), "phase": .string("final_answer"), "item_id": .string("answer")])])]))
                events.append(event)
                let count = try JSONEncoder().encode(event.data).count
                bytes.append(count); total += count
                let removed = TranscriptRetention.removablePrefixCount(byteCounts: bytes, retainedBytes: total, byteLimit: budget)
                total -= bytes.prefix(removed).reduce(0, +)
                events.removeFirst(removed); bytes.removeFirst(removed)
                if index == 1 || index == 513 || index == 600 {
                    let messages = ConversationItem.group(transcript(events)).compactMap(\.message)
                    XCTAssertEqual(messages.count, 1)
                    XCTAssertEqual(messages.first?.text, "Beginning " + String(repeating: "chunk ", count: index - 1))
                    XCTAssertEqual(messages.first?.running, true)
                }
            }
            XCTAssertEqual(events.count, 600)
            let final = "Beginning " + String(repeating: "chunk ", count: 599)
            events.append(try AgentEvent(.object(["cursor": .string("601"), "turn_id": .string("turn"),
                "type": .string("turn_completed"), "final_message": .string(final)])))
            let messages = ConversationItem.group(transcript(events)).compactMap(\.message)
            XCTAssertEqual(messages.map(\.text), [final])
            XCTAssertEqual(messages.first?.running, false)
        }
    }

    func testByteBudgetDropsOldestPayloadsButKeepsNewestOversizedEvent() {
        XCTAssertEqual(TranscriptRetention.removablePrefixCount(byteCounts: [4, 4, 2], retainedBytes: 10, byteLimit: 6), 1)
        XCTAssertEqual(TranscriptRetention.removablePrefixCount(byteCounts: [4, 4, 20], retainedBytes: 28, byteLimit: 6), 2)
        XCTAssertEqual(TranscriptRetention.removablePrefixCount(byteCounts: [], retainedBytes: 0, byteLimit: 6), 0)
    }
}
