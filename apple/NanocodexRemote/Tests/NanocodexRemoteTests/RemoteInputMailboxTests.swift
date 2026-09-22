import XCTest
@testable import NanocodexRemote

final class RemoteInputMailboxTests: XCTestCase {
    private func hover(_ sequence: UInt64, generation: String = "lease") -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        return try! encoder.encode(RemoteInput(kind: .move, sequence: sequence,
            generation: generation, x: 0.5, y: 0.5))
    }

    func testMotionFloodSchedulesOnceAndPreservesReliableBarriers() {
        let mailbox = RemoteInputMailbox()
        var scheduled = 0
        func append(_ value: String, motion: Bool) {
            if mailbox.append(Data(value.utf8), motion: motion) { scheduled += 1 }
        }
        for index in 1...10_000 { if mailbox.append(hover(UInt64(index)), motion: true) { scheduled += 1 } }
        append("button-down", motion: false)
        for index in 10_001...20_000 { if mailbox.append(hover(UInt64(index)), motion: true) { scheduled += 1 } }
        append("relative-1", motion: false)
        append("relative-2", motion: false)
        append("button-up", motion: false)
        guard case .packets(let packets) = mailbox.take() else { return XCTFail("Missing input") }
        XCTAssertEqual(scheduled, 1, "Motion must coalesce before scheduling the main actor")
        XCTAssertEqual(packets.map(\.data), [hover(10_000), Data("button-down".utf8), hover(20_000),
            Data("relative-1".utf8), Data("relative-2".utf8), Data("button-up".utf8)])
        XCTAssertEqual(packets.map(\.motion), [true, false, true, false, false, false])
        // A callback racing with the drain still uses the existing consumer.
        XCTAssertFalse(mailbox.append(Data("release".utf8), motion: false))
        guard case .packets(let final) = mailbox.take() else { return XCTFail("Lost release") }
        XCTAssertEqual(final.count, 1)
        guard case .idle = mailbox.take() else { return XCTFail("Drain did not finish") }
        XCTAssertTrue(mailbox.append(Data("new".utf8), motion: false))
    }

    func testUnorderedMotionKeepsNewestSequenceAndGenerationBoundaries() {
        let mailbox = RemoteInputMailbox()
        XCTAssertTrue(mailbox.append(hover(50), motion: true))
        XCTAssertFalse(mailbox.append(hover(40), motion: true))
        XCTAssertFalse(mailbox.append(hover(1, generation: "new"), motion: true))
        // Invalid motion remains visible to protocol validation, never hidden by coalescing.
        XCTAssertFalse(mailbox.append(Data("invalid".utf8), motion: true))
        XCTAssertFalse(mailbox.append(hover(2, generation: "new"), motion: true))
        guard case .packets(let packets) = mailbox.take() else { return XCTFail("Missing motion") }
        XCTAssertEqual(packets.map(\.data), [hover(50), hover(1, generation: "new"),
            Data("invalid".utf8), hover(2, generation: "new")])
    }

    func testReliableOverflowFailsClosedWithoutReplayingPartialInput() {
        let mailbox = RemoteInputMailbox()
        for index in 0..<256 {
            XCTAssertEqual(mailbox.append(Data([UInt8(index)]), motion: false), index == 0)
        }
        XCTAssertFalse(mailbox.append(Data([0]), motion: false))
        guard case .overflow = mailbox.take() else { return XCTFail("Must fail, not lose a transition") }
        XCTAssertFalse(mailbox.append(Data([1]), motion: false))
        mailbox.close()
        guard case .idle = mailbox.take() else { return XCTFail("Closed mailbox retained input") }
        XCTAssertFalse(mailbox.append(Data([2]), motion: false))
    }

    func testByteBudgetAndOversizedPacketFailClosed() {
        let mailbox = RemoteInputMailbox()
        for _ in 0..<32 { _ = mailbox.append(Data(count: 8192), motion: false) }
        _ = mailbox.append(Data([0]), motion: false)
        guard case .overflow = mailbox.take() else { return XCTFail("Unbounded byte queue") }
        let oversized = RemoteInputMailbox()
        XCTAssertTrue(oversized.append(Data(count: 8193), motion: true))
        guard case .overflow = oversized.take() else { return XCTFail("Oversized motion accepted") }
    }

    func testDrainYieldsInBoundedBatchesAndCloseDiscardsPendingInput() {
        let mailbox = RemoteInputMailbox()
        for index in 0..<65 { _ = mailbox.append(Data([UInt8(index)]), motion: false) }
        for expected in [Array(0..<32), Array(32..<64), [64]] {
            guard case .packets(let packets) = mailbox.take() else { return XCTFail("Missing batch") }
            XCTAssertEqual(packets.map { Int($0.data[0]) }, expected)
        }
        _ = mailbox.append(Data([99]), motion: false)
        mailbox.close()
        guard case .idle = mailbox.take() else { return XCTFail("Replayed input after close") }
    }
}
