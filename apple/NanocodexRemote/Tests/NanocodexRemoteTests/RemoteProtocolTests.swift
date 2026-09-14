import XCTest
@testable import NanocodexRemote
#if os(macOS)
import CoreVideo
import ImageIO
#endif

final class RemoteProtocolTests: XCTestCase {
#if os(macOS)
    func testAgentObservationProducesDecodableBoundedJPEGAndClearsOnStop() throws {
        let observations = RemoteSnapshotBuffer()
        XCTAssertThrowsError(try observations.snapshot())
        var frame: CVPixelBuffer?
        XCTAssertEqual(CVPixelBufferCreate(kCFAllocatorDefault, 2560, 1440, kCVPixelFormatType_32BGRA,
            nil, &frame), kCVReturnSuccess)
        let buffer = try XCTUnwrap(frame)
        CVPixelBufferLockBaseAddress(buffer, [])
        let address = try XCTUnwrap(CVPixelBufferGetBaseAddress(buffer))
        address.initializeMemory(as: UInt8.self, repeating: 255,
            count: CVPixelBufferGetBytesPerRow(buffer) * CVPixelBufferGetHeight(buffer))
        CVPixelBufferUnlockBaseAddress(buffer, [])
        observations.update(buffer)
        let result = try observations.snapshot()
        let source = try XCTUnwrap(CGImageSourceCreateWithData(result.jpeg as CFData, nil))
        let decoded = try XCTUnwrap(CGImageSourceCreateImageAtIndex(source, 0, nil))
        XCTAssertEqual(decoded.width, result.width)
        XCTAssertEqual(decoded.height, result.height)
        XCTAssertEqual(decoded.width, 1280)
        XCTAssertEqual(decoded.height, 720)
        XCTAssertLessThanOrEqual(result.jpeg.count, 500_000)
        observations.clear()
        XCTAssertThrowsError(try observations.snapshot())
    }
#endif
    func testAgentInputIsBoundedAndReleasesModifierKeys() throws {
        let command = RemoteAgentInput(action: "key", key: 4, modifiers: [227])
        let keys = try command.steps(generation: "lease")
        XCTAssertEqual(keys.map { $0.input.key }, [227, 4, 4, 227])
        XCTAssertEqual(keys.map { $0.input.down }, [true, true, false, false])
        let drag = try RemoteAgentInput(action: "drag", x: 0.1, y: 0.2, endX: 0.8, endY: 0.9).steps(generation: "lease")
        XCTAssertLessThanOrEqual(drag.reduce(0) { $0 + $1.delay }, 300)
        XCTAssertEqual(drag.last?.input.down, false)
        XCTAssertThrowsError(try RemoteAgentInput(action: "drag", x: 0, y: 0, endX: .nan, endY: 1).steps(generation: "lease"))
        XCTAssertThrowsError(try RemoteAgentInput(action: "key", key: 40, modifiers: [224, 224]).steps(generation: "lease"))
    }
    func testInputRejectsUnboundedAndMixedCommands() throws {
        let valid = RemoteInput(kind: .button, sequence: 1, generation: "lease-1", x: 0.5, y: 1, button: 0, down: true)
        XCTAssertEqual(try RemoteInput.decode(JSONEncoder().encode(valid)), valid)
        var invalid = valid; invalid.x = .nan
        XCTAssertThrowsError(try invalid.validate())
        invalid = valid; invalid.text = "unrelated input"
        XCTAssertThrowsError(try invalid.validate())
        XCTAssertThrowsError(try RemoteInput.decode(Data(#"{"kind":"releaseAll","sequence":1,"generation":"lease-1","command":"open"}"#.utf8)))
    }

    func testControlGenerationAndDeadlineFenceInput() throws {
        var lease = RemoteControlLease()
        try lease.acquire(owner: "viewer-a", generation: "first", now: 1)
        XCTAssertThrowsError(try lease.acquire(owner: "viewer-b", generation: "second", now: 2))
        XCTAssertThrowsError(try lease.accept(.init(kind: .releaseAll, sequence: 1, generation: "first"), from: "viewer-b", now: 2))
        XCTAssertTrue(lease.isExpired(now: 11))
        XCTAssertThrowsError(try lease.renew(owner: "viewer-a", generation: "first", now: 11))
        lease.release()
        try lease.acquire(owner: "viewer-a", generation: "second", now: 12)
        XCTAssertThrowsError(try lease.accept(.init(kind: .releaseAll, sequence: 1, generation: "first"), from: "viewer-a", now: 12))
    }

    func testReliableKeyUpSurvivesNewerMotionAndClicksFenceStaleMotion() throws {
        var lease = RemoteControlLease()
        try lease.acquire(owner: "a", generation: "g", now: 1)
        XCTAssertTrue(try lease.accept(.init(kind: .move, sequence: 3, generation: "g", x: 0.9, y: 0.9), from: "a", now: 2))
        XCTAssertTrue(try lease.accept(.init(kind: .key, sequence: 2, generation: "g", down: false, key: 4), from: "a", now: 2))
        XCTAssertTrue(try lease.accept(.init(kind: .button, sequence: 5, generation: "g", x: 0.1, y: 0.1, button: 0, down: true), from: "a", now: 2))
        XCTAssertFalse(try lease.accept(.init(kind: .move, sequence: 4, generation: "g", x: 0.9, y: 0.9), from: "a", now: 2))
        XCTAssertFalse(try lease.accept(.init(kind: .button, sequence: 5, generation: "g", x: 0.1, y: 0.1, button: 0, down: true), from: "a", now: 2))
    }
}
