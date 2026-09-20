import XCTest
import Combine
import ImageIO
@testable import NanocodexRemote

@MainActor private final class MotionSocket: RemoteSignalingTransport {
    var onMessage: (RemoteMessage) -> Void = { _ in }
    var onClose: (Error?) -> Void = { _ in }
    var onConnect: () -> Void = {}
    var onSend: (RemoteMessage) -> Void = { _ in }
    var messages: [RemoteMessage] = []
    func connect(hand: RemoteHand?) throws { onConnect() }
    func send(_ message: RemoteMessage) { messages.append(message); onSend(message) }
    func close(error: Error?) { onClose(error) }
    var inputs: [RemoteInput] {
        messages.compactMap { if case .input(let event) = $0.data { return event }; return nil }
    }
    func control(_ type: RemoteControlMessage.Kind, generation: String, gamepad: Bool = false) {
        var message = RemoteMessage(type: "control")
        message.data = .control(.init(type: type, generation: generation, relativePointer: true, gamepad: gamepad))
        onMessage(message)
    }
}

@MainActor private final class MotionFixture {
    let service: RemoteService
    let viewer = RemoteViewer()
    let socket = MotionSocket()

    init() throws {
        service = try RemoteService(origin: URL(string: "https://motion.test")!, configuration: .ephemeral) { _ in }
        viewer.makeSignaling = { [socket] _ in socket }
    }
    func start(gamepad: Bool = false) async throws {
        let hand = try JSONDecoder().decode(RemoteHand.self, from: Data(#"{"id":"desktop","machine_id":"vm:motion","machine_name":"Motion fixture","name":"Desktop","kind":"vm","width":1600,"height":900,"controllable":true,"generation":"publication","transport":"frames-v1"}"#.utf8))
        await viewer.connect(service: service, hand: hand)
        socket.onMessage(.init(type: "ready"))
        let context = try XCTUnwrap(CGContext(data: nil, width: 3, height: 2, bitsPerComponent: 8,
            bytesPerRow: 12, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue))
        let bytes = NSMutableData()
        let destination = try XCTUnwrap(CGImageDestinationCreateWithData(bytes, "public.jpeg" as CFString, 1, nil))
        CGImageDestinationAddImage(destination, try XCTUnwrap(context.makeImage()), nil)
        XCTAssertTrue(CGImageDestinationFinalize(destination))
        var frame = RemoteMessage(type: "frame")
        frame.jpeg = (bytes as Data).base64EncodedString(); frame.width = 3; frame.height = 2
        socket.onMessage(frame)
        for _ in 0..<200 where !viewer.connected { try await Task.sleep(for: .milliseconds(5)) }
        XCTAssertTrue(viewer.connected)
        viewer.takeControl(); socket.control(.granted, generation: "first", gamepad: gamepad)
        XCTAssertTrue(viewer.controlling)
        socket.messages.removeAll()
    }
    func close() { viewer.close(); service.close() }
}

final class RemoteMotionBatchingTests: XCTestCase {
    @MainActor func testGamepadFlushesMotionAndPreservesFullState() async throws {
        let fixture = try MotionFixture(); try await fixture.start(gamepad: true); defer { fixture.close() }
        let state = RemoteGamepadState(leftX: 0.5, rightY: -0.25, leftTrigger: 0.75, buttons: ["a", "rightShoulder"])
        fixture.viewer.input(kind: .relativeMove, deltaX: 3, deltaY: 4)
        fixture.viewer.gamepad(state)
        XCTAssertEqual(fixture.socket.inputs.map(\.kind), [.relativeMove, .gamepad])
        XCTAssertEqual(fixture.socket.inputs.map(\.sequence), [1, 2])
        XCTAssertEqual(fixture.socket.inputs.last?.gamepad, state)
        fixture.viewer.releaseControl()
        XCTAssertEqual(fixture.socket.inputs.last?.gamepad, RemoteGamepadState())
    }

    @MainActor func testBurstIsBatchedWithoutPublishingMovement() async throws {
        let fixture = try MotionFixture(); try await fixture.start(); defer { fixture.close() }
        let sent = expectation(description: "Fixed-deadline batch sent")
        fixture.socket.onSend = { message in
            if case .input(let input) = message.data, input.kind == .relativeMove { sent.fulfill() }
        }
        var publications = 0
        let observer = fixture.viewer.objectWillChange.sink { publications += 1 }
        for _ in 0..<100 { fixture.viewer.input(kind: .relativeMove, deltaX: 1.25, deltaY: -2.5) }
        XCTAssertTrue(fixture.socket.inputs.isEmpty, "A physical burst must not emit one reliable record per sample")
        await fulfillment(of: [sent], timeout: 1)
        XCTAssertEqual(fixture.socket.inputs.count, 1)
        XCTAssertEqual(fixture.socket.inputs.first?.deltaX, 125)
        XCTAssertEqual(fixture.socket.inputs.first?.deltaY, -250)
        XCTAssertEqual(publications, 0)
        withExtendedLifetime(observer) {}
    }

    @MainActor func testEveryNonRelativeInputFlushesBeforeItsOwnRecord() async throws {
        let fixture = try MotionFixture(); try await fixture.start(); defer { fixture.close() }
        let viewer = fixture.viewer
        let boundaries: [(RemoteInput.Kind, () -> Void)] = [
            (.button, { viewer.input(kind: .button, button: 1, down: true) }),
            (.button, { viewer.input(kind: .button, button: 0, down: true) }),
            (.scroll, { viewer.input(kind: .scroll, deltaX: 0, deltaY: 20) }),
            (.key, { viewer.input(kind: .key, down: true, key: 26) }),
            (.text, { viewer.input(kind: .text, text: "fixture") }),
            (.move, { viewer.input(kind: .move, x: 0.5, y: 0.5) }),
            (.button, { viewer.input(kind: .button, button: 1, down: false) }),
            (.button, { viewer.input(kind: .button, button: 0, down: false) }),
            (.releaseAll, { viewer.input(kind: .releaseAll) })
        ]
        for (_, send) in boundaries {
            viewer.input(kind: .relativeMove, deltaX: 12, deltaY: -4)
            viewer.input(kind: .relativeMove, deltaX: 3, deltaY: 2)
            send()
        }
        let inputs = fixture.socket.inputs
        XCTAssertEqual(inputs.map(\.kind), boundaries.flatMap { [.relativeMove, $0.0] })
        XCTAssertEqual(inputs.map(\.sequence), inputs.indices.map { UInt64($0 + 1) })
        for input in inputs where input.kind == .relativeMove {
            XCTAssertEqual(input.deltaX, 15); XCTAssertEqual(input.deltaY, -2)
        }
        for input in inputs { XCTAssertNoThrow(try input.validate()) }
        let count = inputs.count
        try await Task.sleep(for: .milliseconds(20))
        XCTAssertEqual(fixture.socket.inputs.count, count, "Flushed timers must not emit duplicate movement")
    }

    @MainActor func testLargeAggregateSplitsWithoutLosingDisplacement() async throws {
        let fixture = try MotionFixture(); try await fixture.start(); defer { fixture.close() }
        for _ in 0..<4 { fixture.viewer.input(kind: .relativeMove, deltaX: 3000.25, deltaY: -2250.5) }
        fixture.viewer.input(kind: .button, button: 1, down: false)
        let movement = fixture.socket.inputs.filter { $0.kind == .relativeMove }
        XCTAssertEqual(movement.count, 3)
        XCTAssertEqual(movement.compactMap(\.deltaX), [4096, 4096, 3809])
        XCTAssertEqual(movement.compactMap(\.deltaY), [-4096, -4096, -810])
        XCTAssertEqual(movement.compactMap(\.deltaX).reduce(0, +), 12001)
        XCTAssertEqual(movement.compactMap(\.deltaY).reduce(0, +), -9002)
        XCTAssertEqual(fixture.socket.inputs.last?.kind, .button)
        for input in fixture.socket.inputs { XCTAssertNoThrow(try input.validate()) }
    }

    @MainActor func testExplicitControlReleaseFlushesBeforeReleaseMessage() async throws {
        let fixture = try MotionFixture(); try await fixture.start(); defer { fixture.close() }
        fixture.viewer.input(kind: .relativeMove, deltaX: 7, deltaY: 9)
        fixture.viewer.releaseControl()
        XCTAssertEqual(fixture.socket.messages.map(\.type), ["input", "control"])
        XCTAssertEqual(fixture.socket.inputs.first?.deltaX, 7)
        guard case .control(let release) = fixture.socket.messages.last?.data else { return XCTFail("Missing control release") }
        XCTAssertEqual(release.type, .release); XCTAssertEqual(release.generation, "first")
        XCTAssertFalse(fixture.viewer.controlling)
        try await Task.sleep(for: .milliseconds(20))
        XCTAssertEqual(fixture.socket.inputs.count, 1)
    }

    @MainActor func testRevocationDiscardsPendingMovementBeforeNewGeneration() async throws {
        let fixture = try MotionFixture(); try await fixture.start(); defer { fixture.close() }
        fixture.viewer.input(kind: .relativeMove, deltaX: 99, deltaY: -80)
        fixture.socket.control(.revoked, generation: "first")
        XCTAssertFalse(fixture.viewer.controlling)
        fixture.viewer.takeControl(); fixture.socket.control(.granted, generation: "second")
        fixture.viewer.input(kind: .relativeMove, deltaX: 2, deltaY: 3)
        fixture.viewer.input(kind: .releaseAll)
        try await Task.sleep(for: .milliseconds(20))
        let inputs = fixture.socket.inputs
        XCTAssertEqual(inputs.map(\.kind), [.relativeMove, .releaseAll])
        XCTAssertEqual(inputs.map(\.generation), ["second", "second"])
        XCTAssertEqual(inputs.map(\.sequence), [1, 2])
        XCTAssertEqual(inputs.first?.deltaX, 2); XCTAssertEqual(inputs.first?.deltaY, 3)
    }

    @MainActor func testDisconnectDiscardsPendingMovement() async throws {
        let fixture = try MotionFixture(); try await fixture.start(); defer { fixture.close() }
        fixture.viewer.input(kind: .relativeMove, deltaX: 99, deltaY: -80)
        fixture.viewer.suspend()
        try await Task.sleep(for: .milliseconds(20))
        XCTAssertTrue(fixture.socket.inputs.isEmpty)
        XCTAssertFalse(fixture.viewer.controlling)
        XCTAssertFalse(fixture.viewer.connected)
    }

    @MainActor func testInvalidSampleCannotEnterTheBatch() async throws {
        let fixture = try MotionFixture(); try await fixture.start(); defer { fixture.close() }
        fixture.viewer.input(kind: .relativeMove, deltaX: 10, deltaY: 20)
        fixture.viewer.input(kind: .relativeMove, deltaX: .nan, deltaY: 0)
        try await Task.sleep(for: .milliseconds(20))
        XCTAssertTrue(fixture.socket.inputs.isEmpty)
        XCTAssertFalse(fixture.viewer.controlling)
        XCTAssertEqual(fixture.viewer.status, RemoteError.invalidMessage.localizedDescription)
    }
}
