import XCTest
@testable import NanocodexRemote

final class RemoteSendQueueTests: XCTestCase {
    private func event(_ kind: RemoteInput.Kind, _ sequence: UInt64, generation: String = "lease", down: Bool? = nil) -> RemoteMessage {
        var message = RemoteMessage(type: "input")
        message.data = .input(RemoteInput(kind: kind, sequence: sequence, generation: generation,
            x: [.move, .button, .scroll].contains(kind) ? 0.5 : nil,
            y: [.move, .button, .scroll].contains(kind) ? 0.5 : nil, button: kind == .button ? 0 : nil, down: down,
            key: kind == .key ? 4 : nil, text: kind == .text ? "hello" : nil,
            deltaX: kind == .scroll ? 1 : nil, deltaY: kind == .scroll ? 2 : nil))
        return message
    }
    private func sequence(_ message: RemoteMessage?) -> UInt64? {
        guard case .input(let input) = message?.data else { return nil }
        return input.sequence
    }

    func testMoveBurstDoesNotDelayKeyOrButtonRelease() {
        var queue = RemoteSendQueue()
        XCTAssertTrue(queue.append(event(.key, 1, down: true)))
        XCTAssertTrue(queue.append(event(.button, 2, down: true)))
        for sequence in 3...10_002 { XCTAssertTrue(queue.append(event(.move, UInt64(sequence)))) }
        XCTAssertTrue(queue.append(event(.button, 10_003, down: false)))
        XCTAssertTrue(queue.append(event(.key, 10_004, down: false)))
        XCTAssertEqual(queue.count, 5)
        XCTAssertEqual((0..<5).compactMap { _ in sequence(queue.popFirst()) }, [1, 2, 10_002, 10_003, 10_004])
        XCTAssertNil(queue.popFirst())
    }

    func testControlMessagesAndGenerationsAreBarriers() {
        var queue = RemoteSendQueue()
        XCTAssertTrue(queue.append(event(.move, 1)))
        var release = RemoteMessage(type: "control")
        release.data = .control(.init(type: .release, generation: "lease"))
        XCTAssertTrue(queue.append(release))
        XCTAssertTrue(queue.append(event(.move, 2)))
        XCTAssertTrue(queue.append(event(.move, 3, generation: "next")))
        XCTAssertEqual(sequence(queue.popFirst()), 1)
        XCTAssertEqual(queue.popFirst()?.type, "control")
        XCTAssertEqual(sequence(queue.popFirst()), 2)
        XCTAssertEqual(sequence(queue.popFirst()), 3)
    }

    func testBoundKeepsDiscreteEventsAndAllowsLatestMoveAtCapacity() {
        var queue = RemoteSendQueue()
        XCTAssertTrue(queue.append(event(.key, 1, down: true), limit: 2))
        XCTAssertTrue(queue.append(event(.move, 3), limit: 2))
        XCTAssertTrue(queue.append(event(.move, 2), limit: 2))
        XCTAssertTrue(queue.append(event(.move, 4), limit: 2))
        XCTAssertFalse(queue.append(event(.key, 5, down: false), limit: 2))
        XCTAssertEqual(sequence(queue.popFirst()), 1)
        XCTAssertEqual(sequence(queue.popFirst()), 4)
    }

    func testEveryNonMoveInputIsABarrier() throws {
        for kind: RemoteInput.Kind in [.button, .key, .scroll, .text, .releaseAll] {
            var queue = RemoteSendQueue()
            let barrier = event(kind, 2, down: [.button, .key].contains(kind) ? false : nil)
            if case .input(let input) = barrier.data { try input.validate() }
            XCTAssertTrue(queue.append(event(.move, 1)))
            XCTAssertTrue(queue.append(barrier))
            XCTAssertTrue(queue.append(event(.move, 3)))
            XCTAssertEqual((0..<3).compactMap { _ in sequence(queue.popFirst()) }, [1, 2, 3])
        }
    }

    func testMoveBurstCPU() {
        measure(metrics: [XCTClockMetric(), XCTCPUMetric()]) {
            var queue = RemoteSendQueue()
            for sequence in 1...10_000 { _ = queue.append(event(.move, UInt64(sequence))) }
            XCTAssertEqual(queue.count, 1)
        }
    }
}
