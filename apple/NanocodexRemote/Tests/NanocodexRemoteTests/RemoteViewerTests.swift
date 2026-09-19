import XCTest
import Combine
import ImageIO
@testable import NanocodexRemote

private final class RemoteHTTPFixture: URLProtocol {
    static let lock = NSLock()
    static var handler: ((RemoteHTTPFixture) -> Void)?
    override class func canInit(with request: URLRequest) -> Bool { request.url?.host == "remote.test" }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() { Self.lock.withLock { Self.handler }?(self) }
    override func stopLoading() {}
    func respond(_ status: Int, _ value: [String: Any] = [:]) {
        client?.urlProtocol(self, didReceive: HTTPURLResponse(url: request.url!, statusCode: status,
            httpVersion: "HTTP/1.1", headerFields: ["Content-Type": "application/json"])!, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: try! JSONSerialization.data(withJSONObject: value))
        client?.urlProtocolDidFinishLoading(self)
    }
}

@MainActor private final class ViewerSocket: RemoteSignalingTransport {
    var onMessage: (RemoteMessage) -> Void = { _ in }
    var onClose: (Error?) -> Void = { _ in }
    var onConnect: () -> Void = {}
    var onSend: (RemoteMessage) -> Void = { _ in }
    var messages: [RemoteMessage] = []
    var closed = false
    func connect(hand: RemoteHand?) throws { onConnect() }
    func send(_ message: RemoteMessage) { messages.append(message); onSend(message) }
    func close(error: Error?) { closed = true; onClose(error) }
}

private final class FrameDecodeGate: @unchecked Sendable {
    let entered: XCTestExpectation
    let release = DispatchSemaphore(value: 0)
    private let lock = NSLock()
    private var calls = 0
    private var active = 0
    private var maximum = 0
    private var usedMain = false
    init(entered: XCTestExpectation) { self.entered = entered }
    var counts: (calls: Int, maximum: Int, usedMain: Bool) {
        lock.withLock { (calls, maximum, usedMain) }
    }
    func decode(_ message: RemoteMessage) throws -> CGImage {
        let first = lock.withLock {
            calls += 1; active += 1; maximum = max(maximum, active)
            usedMain = usedMain || Thread.isMainThread
            return calls == 1
        }
        defer { lock.withLock { active -= 1 } }
        if first { entered.fulfill(); _ = release.wait(timeout: .now() + 5) }
        return try RemoteFrame.decode(message)
    }
}

final class RemoteViewerTests: XCTestCase {
    @MainActor func testBroadcastStoppingBlocksMutationsAndPollsUntilStopped() async throws {
        let service = try service { _ in XCTFail("Frame transport must not fetch ICE") }
        defer { service.close() }
        var catalog = surface("broadcast-stopping")
        catalog["transport"] = "frames-v1"; catalog["broadcast"] = true
        let hand = try JSONDecoder().decode(RemoteHand.self, from: JSONSerialization.data(withJSONObject: catalog))
        let socket = ViewerSocket()
        socket.onConnect = { socket.onMessage(.init(type: "ready")) }
        let viewer = RemoteViewer(); viewer.makeSignaling = { _ in socket }
        defer { viewer.close() }
        await viewer.connect(service: service, hand: hand)
        let initial = try XCTUnwrap(socket.messages.last { $0.type == "broadcast" })
        var stopping = RemoteMessage(type: "broadcast_result")
        stopping.requestID = initial.requestID; stopping.agentStatus = "stopping"
        socket.onMessage(stopping)
        XCTAssertEqual(viewer.broadcastStatus, "stopping")
        XCTAssertFalse(viewer.broadcastWaiting)
        let count = socket.messages.count
        viewer.broadcast(action: "start", url: "rtmp://127.0.0.1/live/test")
        viewer.broadcast(action: "stop")
        XCTAssertEqual(socket.messages.count, count)
        let context = try XCTUnwrap(CGContext(data: nil, width: 3, height: 2, bitsPerComponent: 8,
            bytesPerRow: 12, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue))
        let bytes = NSMutableData()
        let destination = try XCTUnwrap(CGImageDestinationCreateWithData(bytes, "public.jpeg" as CFString, 1, nil))
        CGImageDestinationAddImage(destination, try XCTUnwrap(context.makeImage()), nil)
        XCTAssertTrue(CGImageDestinationFinalize(destination))
        var frame = RemoteMessage(type: "frame")
        frame.jpeg = (bytes as Data).base64EncodedString(); frame.width = 3; frame.height = 2
        // Keep frame transport alive while exercising the real five-second poll.
        for _ in 0..<6 {
            socket.onMessage(frame)
            try await Task.sleep(for: .seconds(1))
        }
        let poll = try XCTUnwrap(socket.messages.last { $0.type == "broadcast" })
        XCTAssertEqual(poll.action, "status"); XCTAssertNotEqual(poll.requestID, initial.requestID)
        var stopped = RemoteMessage(type: "broadcast_result")
        stopped.requestID = poll.requestID; stopped.agentStatus = "stopped"
        socket.onMessage(stopped)
        XCTAssertEqual(viewer.broadcastStatus, "stopped")
        XCTAssertFalse(viewer.broadcastWaiting)
    }

    @MainActor func testFrameWindowRefillsAfterDecodeAndStopsOnSuspend() async throws {
        let service = try service { _ in XCTFail("Frame transport must not fetch ICE") }
        defer { service.close() }
        var catalog = surface("window")
        catalog["transport"] = "frames-v1"; catalog["frame_window"] = 6
        let hand = try JSONDecoder().decode(RemoteHand.self, from: JSONSerialization.data(withJSONObject: catalog))
        let socket = ViewerSocket()
        socket.onConnect = { socket.onMessage(.init(type: "ready")) }
        let viewer = RemoteViewer()
        viewer.makeSignaling = { _ in socket }
        defer { viewer.close() }
        await viewer.connect(service: service, hand: hand)
        XCTAssertTrue(socket.messages.isEmpty, "Initial credits are part of the viewer upgrade")
        let context = try XCTUnwrap(CGContext(data: nil, width: 3, height: 2, bitsPerComponent: 8,
            bytesPerRow: 12, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue))
        let bytes = NSMutableData()
        let destination = try XCTUnwrap(CGImageDestinationCreateWithData(bytes, "public.jpeg" as CFString, 1, nil))
        CGImageDestinationAddImage(destination, try XCTUnwrap(context.makeImage()), nil)
        XCTAssertTrue(CGImageDestinationFinalize(destination))
        var frame = RemoteMessage(type: "frame")
        frame.jpeg = (bytes as Data).base64EncodedString(); frame.width = 3; frame.height = 2
        let decoded = expectation(description: "Six credits returned after worker decode")
        decoded.expectedFulfillmentCount = 6
        socket.onSend = { if $0.type == "frame_request" { decoded.fulfill() } }
        for _ in 0..<6 { socket.onMessage(frame) }
        XCTAssertTrue(socket.messages.isEmpty, "Reception alone does not replenish credits")
        await fulfillment(of: [decoded], timeout: 3)
        XCTAssertTrue(viewer.connected)
        XCTAssertEqual(socket.messages.filter { $0.type == "frame_request" }.map(\.count), [1, 1, 1, 1, 1, 1])
        let lateFrame = socket.onMessage
        viewer.suspend(); lateFrame(frame)
        XCTAssertNil(viewer.frame)
        XCTAssertEqual(socket.messages.filter { $0.type == "frame_request" }.count, 6)
    }

    @MainActor func testRelativePointerRequiresCurrentExplicitGrant() async throws {
        let service = try service { _ in XCTFail("No ICE for frames") }
        defer { service.close() }
        var catalog = surface("pointer-capability")
        catalog["transport"] = "frames-v1"; catalog["frame_window"] = 6
        let hand = try JSONDecoder().decode(RemoteHand.self, from: JSONSerialization.data(withJSONObject: catalog))
        let socket = ViewerSocket(), viewer = RemoteViewer()
        socket.onConnect = { socket.onMessage(.init(type: "ready")) }
        viewer.makeSignaling = { _ in socket }
        defer { viewer.close() }
        await viewer.connect(service: service, hand: hand)
        let ready = expectation(description: "Decoded frame connects viewer")
        socket.onSend = { if $0.type == "frame_request" { ready.fulfill() } }
        socket.onMessage(try jpegFrame())
        await fulfillment(of: [ready], timeout: 3)
        socket.onSend = { _ in }
        func deliver(_ control: RemoteControlMessage) {
            var message = RemoteMessage(type: "control"); message.data = .control(control)
            socket.onMessage(message)
        }
        func relativeInputs() {
            viewer.input(kind: .relativeMove, deltaX: 2, deltaY: 3)
            viewer.input(kind: .button, button: 2, down: true)
            viewer.input(kind: .scroll, deltaX: 0, deltaY: 1)
        }
        XCTAssertFalse(viewer.supportsRelativePointer)
        var changes = 0
        let metadataObserver = viewer.objectWillChange.sink { changes += 1 }
        defer { metadataObserver.cancel() }
        // Absent capability is the deployed older-host wire format.
        for capability: Bool? in [nil, false, true] {
            viewer.takeControl()
            let changesBeforeGrant = changes
            let grant = RemoteControlMessage(type: .granted, generation: "lease", relativePointer: capability)
            deliver(try JSONDecoder().decode(RemoteControlMessage.self, from: JSONEncoder().encode(grant)))
            XCTAssertTrue(viewer.controlling)
            XCTAssertGreaterThan(changes, changesBeforeGrant, "Control grants must update SwiftUI metadata observers")
            XCTAssertEqual(viewer.supportsRelativePointer, capability == true)
            let before = socket.messages.count
            relativeInputs()
            XCTAssertEqual(socket.messages.count - before, capability == true ? 3 : 0)
            viewer.input(kind: .button, x: 0.5, y: 0.5, button: 0, down: true)
            XCTAssertEqual(socket.messages.count - before, capability == true ? 4 : 1,
                "Absolute pointer input stays compatible with older hosts")
            deliver(.init(type: .revoked, generation: "stale"))
            XCTAssertEqual(viewer.supportsRelativePointer, capability == true)
            XCTAssertTrue(viewer.controlling)
            deliver(.init(type: .revoked, generation: "lease"))
            XCTAssertFalse(viewer.supportsRelativePointer)
            XCTAssertFalse(viewer.controlling)
            let revokedCount = socket.messages.count
            relativeInputs()
            XCTAssertEqual(socket.messages.count, revokedCount)
        }
        viewer.takeControl()
        deliver(.init(type: .granted, generation: "release", relativePointer: true))
        viewer.releaseControl()
        XCTAssertFalse(viewer.supportsRelativePointer)
        deliver(.init(type: .revoked))
        viewer.takeControl()
        viewer.releaseControl()
        deliver(.init(type: .granted, generation: "cancelled", relativePointer: true))
        XCTAssertFalse(viewer.supportsRelativePointer, "A cancelled acquire cannot enable pointer input")
        deliver(.init(type: .revoked))
        viewer.takeControl()
        deliver(.init(type: .granted, generation: "disconnect", relativePointer: true))
        XCTAssertTrue(viewer.supportsRelativePointer)
        viewer.suspend()
        XCTAssertFalse(viewer.supportsRelativePointer)
    }

    private func jpegFrame(width: Int = 3) throws -> RemoteMessage {
        let context = try XCTUnwrap(CGContext(data: nil, width: width, height: 2, bitsPerComponent: 8,
            bytesPerRow: width * 4, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue))
        let bytes = NSMutableData()
        let destination = try XCTUnwrap(CGImageDestinationCreateWithData(bytes, "public.jpeg" as CFString, 1, nil))
        CGImageDestinationAddImage(destination, try XCTUnwrap(context.makeImage()), nil)
        XCTAssertTrue(CGImageDestinationFinalize(destination))
        var message = RemoteMessage(type: "frame")
        message.jpeg = (bytes as Data).base64EncodedString(); message.width = width; message.height = 2
        return message
    }

    @MainActor func testFrameWorkerPublishesFIFOAndReturnsCreditAfterPublication() async throws {
        let service = try service { _ in XCTFail("No ICE for frames") }
        defer { service.close() }
        var catalog = surface("fifo")
        catalog["transport"] = "frames-v1"; catalog["frame_window"] = 6
        let hand = try JSONDecoder().decode(RemoteHand.self, from: JSONSerialization.data(withJSONObject: catalog))
        let socket = ViewerSocket(), viewer = RemoteViewer()
        socket.onConnect = { socket.onMessage(.init(type: "ready")) }
        viewer.makeSignaling = { _ in socket }
        defer { viewer.close() }
        await viewer.connect(service: service, hand: hand)
        var widths: [Int] = []
        let observer = viewer.$frame.sink { if let image = $0 { widths.append(image.width) } }
        defer { observer.cancel() }
        let completed = expectation(description: "FIFO batch returns six credits")
        completed.expectedFulfillmentCount = 6
        socket.onSend = { message in
            guard message.type == "frame_request" else { return }
            XCTAssertEqual(widths.count, socket.messages.count, "Publish each frame before returning its credit")
            completed.fulfill()
        }
        for width in 1...6 { socket.onMessage(try jpegFrame(width: width)) }
        await fulfillment(of: [completed], timeout: 3)
        XCTAssertEqual(widths, [1, 2, 3, 4, 5, 6])
        XCTAssertEqual(socket.messages.map(\.count), [1, 1, 1, 1, 1, 1])
    }

    @MainActor func testLegacyFrameRequestsKeepThirtyFPSPacing() async throws {
        let service = try service { _ in XCTFail("No ICE for frames") }
        defer { service.close() }
        var catalog = surface("legacy-pacing")
        catalog["transport"] = "frames-v1"
        let hand = try JSONDecoder().decode(RemoteHand.self, from: JSONSerialization.data(withJSONObject: catalog))
        let socket = ViewerSocket(), viewer = RemoteViewer()
        socket.onConnect = { socket.onMessage(.init(type: "ready")) }
        viewer.makeSignaling = { _ in socket }
        defer { viewer.close() }
        var requestedAt: [TimeInterval] = []
        let completed = expectation(description: "Legacy request replenishes after pacing interval")
        socket.onSend = { message in
            guard message.type == "frame_request" else { return }
            requestedAt.append(ProcessInfo.processInfo.systemUptime)
            XCTAssertNil(message.count, "Legacy requests retain their original wire envelope")
            if requestedAt.count == 2 { completed.fulfill() }
        }
        await viewer.connect(service: service, hand: hand)
        XCTAssertEqual(requestedAt.count, 1)
        socket.onMessage(try jpegFrame())
        await fulfillment(of: [completed], timeout: 3)
        XCTAssertEqual(requestedAt.count, 2)
        if requestedAt.count == 2 {
            XCTAssertGreaterThanOrEqual(requestedAt[1] - requestedAt[0], 1.0 / 30.0 - 0.002)
        }
    }

    @MainActor func testSteadyFramesPublishWithoutInvalidatingSwiftUIViewer() async throws {
        let service = try service { _ in XCTFail("No ICE for frames") }
        defer { service.close() }
        var catalog = surface("publication")
        catalog["transport"] = "frames-v1"; catalog["frame_window"] = 6
        let hand = try JSONDecoder().decode(RemoteHand.self, from: JSONSerialization.data(withJSONObject: catalog))
        let socket = ViewerSocket(), viewer = RemoteViewer()
        socket.onConnect = { socket.onMessage(.init(type: "ready")) }
        viewer.makeSignaling = { _ in socket }
        defer { viewer.close() }
        await viewer.connect(service: service, hand: hand)
        let frame = try jpegFrame()
        let first = expectation(description: "First decoded frame finishes connection state changes")
        socket.onSend = { if $0.type == "frame_request" { first.fulfill() } }
        socket.onMessage(frame)
        await fulfillment(of: [first], timeout: 3)
        XCTAssertTrue(viewer.connected)

        var invalidations = 0, publications = 0
        let changes = viewer.objectWillChange.sink { invalidations += 1 }
        // CurrentValueSubject immediately replays the existing frame; measure
        // only subsequent emissions after the connection has settled.
        let images = viewer.$frame.dropFirst().sink { if $0 != nil { publications += 1 } }
        defer { changes.cancel(); images.cancel() }
        let decoded = expectation(description: "Six steady frames decode and return credit")
        decoded.expectedFulfillmentCount = 6
        socket.onSend = { if $0.type == "frame_request" { decoded.fulfill() } }
        for _ in 0..<6 { socket.onMessage(frame) }
        await fulfillment(of: [decoded], timeout: 3)
        XCTAssertEqual(publications, 6)
        XCTAssertEqual(invalidations, 0, "Steady JPEG publication must not rebuild SwiftUI observers")
        viewer.suspend()
        XCTAssertGreaterThan(invalidations, 0, "Connection state must still invalidate SwiftUI")
    }

    @MainActor func testFrameWorkerBoundsCreditsAndRejectsUnsolicitedFrame() async throws {
        let service = try service { _ in XCTFail("No ICE for frames") }
        defer { service.close() }
        var catalog = surface("bounded")
        catalog["transport"] = "frames-v1"; catalog["frame_window"] = 6
        let hand = try JSONDecoder().decode(RemoteHand.self, from: JSONSerialization.data(withJSONObject: catalog))
        let socket = ViewerSocket(), viewer = RemoteViewer()
        socket.onConnect = { socket.onMessage(.init(type: "ready")) }
        viewer.makeSignaling = { _ in socket }
        let entered = expectation(description: "Worker started")
        let gate = FrameDecodeGate(entered: entered)
        viewer.frameDecoder = RemoteFrameDecoder { try gate.decode($0) }
        defer { gate.release.signal(); viewer.close() }
        await viewer.connect(service: service, hand: hand)
        let frame = try jpegFrame()
        for _ in 0..<6 { socket.onMessage(frame) }
        await fulfillment(of: [entered], timeout: 2)
        XCTAssertEqual(gate.counts.calls, 1)
        XCTAssertFalse(gate.counts.usedMain)
        XCTAssertTrue(socket.messages.isEmpty)
        socket.onMessage(frame)
        XCTAssertTrue(socket.closed)
        XCTAssertEqual(viewer.status, RemoteError.invalidMessage.localizedDescription)
        XCTAssertNil(viewer.frame)
    }

    @MainActor func testFrameWorkerFencesSuspendReconnectAndStaleDecodeResults() async throws {
        for invalidOldFrame in [false, true] {
            let service = try service { _ in XCTFail("No ICE for frames") }
            defer { service.close() }
            var catalog = surface("epoch")
            catalog["transport"] = "frames-v1"; catalog["frame_window"] = 6
            let hand = try JSONDecoder().decode(RemoteHand.self, from: JSONSerialization.data(withJSONObject: catalog))
            let old = ViewerSocket(), fresh = ViewerSocket(), viewer = RemoteViewer()
            old.onConnect = { old.onMessage(.init(type: "ready")) }
            fresh.onConnect = { fresh.onMessage(.init(type: "ready")) }
            viewer.makeSignaling = { _ in old }
            let gate = FrameDecodeGate(entered: expectation(description: "Old worker started"))
            viewer.frameDecoder = RemoteFrameDecoder { try gate.decode($0) }
            defer { gate.release.signal(); viewer.close() }
            await viewer.connect(service: service, hand: hand)
            // Both successful old images and old decode failures must be fenced.
            old.onMessage(invalidOldFrame ? .init(type: "frame") : try jpegFrame())
            await fulfillment(of: [gate.entered], timeout: 2)
            let stale = old.onMessage
            viewer.suspend()
            viewer.makeSignaling = { _ in fresh }
            await viewer.connect(service: service, hand: hand)
            let frame = try jpegFrame()
            var published = 0
            let observer = viewer.$frame.sink { if $0 != nil { published += 1 } }
            defer { observer.cancel() }
            let completed = expectation(description: "Fresh epoch returns exactly six credits")
            completed.expectedFulfillmentCount = 6
            fresh.onSend = { if $0.type == "frame_request" { completed.fulfill() } }
            for _ in 0..<6 { fresh.onMessage(frame) }
            stale(frame)
            XCTAssertNil(viewer.frame)
            XCTAssertTrue(fresh.messages.isEmpty)
            XCTAssertEqual(gate.counts.calls, 1)
            gate.release.signal()
            await fulfillment(of: [completed], timeout: 3)
            XCTAssertTrue(viewer.connected)
            XCTAssertFalse(fresh.closed)
            XCTAssertNotNil(viewer.frame)
            XCTAssertEqual(gate.counts.calls, 7)
            XCTAssertEqual(gate.counts.maximum, 1)
            XCTAssertFalse(gate.counts.usedMain)
            XCTAssertTrue(old.messages.isEmpty)
            XCTAssertEqual(published, 6, "The stale successful decode must not publish")
        }
    }

    @MainActor func testNativeZoomKeepsScreenCoordinatesStable() {
        let viewer = RemoteViewer()
        #if os(macOS)
        let viewport = MacRemoteViewport(viewer: viewer)
        viewport.frame = CGRect(x: 0, y: 0, width: 640, height: 360)
        viewport.layoutSubtreeIfNeeded()
        let original = viewport.canvas.frame.size
        viewport.setMagnification(2, centeredAt: CGPoint(x: 320, y: 180))
        viewport.layoutSubtreeIfNeeded()
        XCTAssertEqual(viewport.magnification, 2, accuracy: 0.01)
        XCTAssertEqual(viewport.canvas.frame.size, original, "Magnification changes the viewport, not the remote document's coordinates")
        #else
        let canvas = TouchRemoteCanvas(viewer: viewer)
        canvas.frame = CGRect(x: 0, y: 0, width: 640, height: 360)
        canvas.layoutIfNeeded()
        XCTAssertEqual(canvas.normalizedPoint(CGPoint(x: 320, y: 180)), CGPoint(x: 0.5, y: 0.5))
        canvas.viewport.setZoomScale(2, animated: false)
        canvas.viewport.contentOffset = CGPoint(x: 320, y: 180)
        XCTAssertEqual(canvas.normalizedPoint(CGPoint(x: 320, y: 180)), CGPoint(x: 0.5, y: 0.5))
        #endif
    }

    @MainActor func testCanvasTeardownDoesNotPublishDuringSwiftUIInvalidation() {
        let viewer = RemoteViewer()
#if os(macOS)
        let canvas = MacRemoteCanvas(viewer: viewer)
#else
        let canvas = TouchRemoteCanvas(viewer: viewer)
#endif
        var changes = 0
        let observer = viewer.objectWillChange.sink { changes += 1 }
        canvas.detach()
        XCTAssertEqual(changes, 0)
        withExtendedLifetime(observer) {}
    }

    override func tearDown() {
        RemoteHTTPFixture.lock.withLock { RemoteHTTPFixture.handler = nil }
        super.tearDown()
    }

    private func surface(_ generation: String, machine: String = "vm:test") -> [String: Any] {
        ["id": "desktop", "machine_id": machine, "machine_name": "Test VM", "name": "Desktop",
         "kind": "vm", "width": 1600, "height": 900, "controllable": true, "generation": generation]
    }

    private func hand(_ generation: String, machine: String = "vm:test") throws -> RemoteHand {
        try JSONDecoder().decode(RemoteHand.self, from: JSONSerialization.data(withJSONObject: surface(generation, machine: machine)))
    }

    private func service(_ handler: @escaping (RemoteHTTPFixture) -> Void) throws -> RemoteService {
        RemoteHTTPFixture.lock.withLock { RemoteHTTPFixture.handler = handler }
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [RemoteHTTPFixture.self]
        return try RemoteService(origin: URL(string: "https://remote.test")!, configuration: configuration) {
            $0.setValue("Bearer fixture", forHTTPHeaderField: "Authorization")
        }
    }

    @MainActor private func viewer(recoveryWindow: Duration = .seconds(90)) -> RemoteViewer {
        let viewer = RemoteViewer(recoveryWindow: recoveryWindow)
        viewer.makeSignaling = { _ in ViewerSocket() }
        return viewer
    }

    @MainActor func testInitialICEOverlapsSignalingAndIsReusedUntilRestart() async throws {
        let initialICE = expectation(description: "Initial ICE request started")
        let socketOpened = expectation(description: "Socket opened while ICE is outstanding")
        let firstAnswer = expectation(description: "Initial offer answered")
        let restartAnswer = expectation(description: "Restart answered with renewed ICE")
        let prematureAnswer = expectation(description: "No answer before ICE authorization")
        prematureAnswer.isInverted = true
        let lock = NSLock()
        var pending: RemoteHTTPFixture?, requests = 0
        let service = try service { request in
            let count = lock.withLock { requests += 1; return requests }
            if count == 1 { lock.withLock { pending = request }; initialICE.fulfill() }
            else { request.respond(200, ["iceServers": []]) }
        }
        let viewer = viewer(), socket = ViewerSocket(), publisher = try RemotePeer(publishing: true, ice: [])
        var publisherQueue: Task<Void, Never>?, answers = 0, credentialsReturned = false
        defer { viewer.close(); publisher.close(); publisherQueue?.cancel(); service.close() }
        viewer.makeSignaling = { _ in socket }
        socket.onConnect = { socketOpened.fulfill() }
        publisher.onSignal = { socket.onMessage(.init(type: "signal", signal: $0)) }
        socket.onSend = { message in
            guard let signal = message.signal else { return }
            if signal.type == .answer && !credentialsReturned { prematureAnswer.fulfill() }
            let previous = publisherQueue
            publisherQueue = Task {
                await previous?.value
                do {
                    try await publisher.receive(signal)
                    if signal.type == .answer {
                        answers += 1
                        if answers == 1 { firstAnswer.fulfill() }
                        if answers == 2 { restartAnswer.fulfill() }
                    }
                } catch { XCTFail("Publisher signaling failed: \(error)") }
            }
        }
        let selected = try hand("original")
        let connection = Task { await viewer.connect(service: service, hand: selected) }
        await fulfillment(of: [initialICE, socketOpened], timeout: 2)
        try await publisher.offer()
        await fulfillment(of: [prematureAnswer], timeout: 0.1)
        XCTAssertNil(viewer.track)
        XCTAssertFalse(viewer.connected)
        credentialsReturned = true
        lock.withLock { pending }?.respond(200, ["iceServers": []])
        await connection.value
        await fulfillment(of: [firstAnswer], timeout: 5)
        XCTAssertEqual(lock.withLock { requests }, 1, "The first offer must reuse initial ICE credentials")
        try await publisher.restartICE([])
        await fulfillment(of: [restartAnswer], timeout: 5)
        XCTAssertEqual(lock.withLock { requests }, 2, "A subsequent offer must fetch fresh ICE credentials")
        XCTAssertFalse(viewer.controlling)
        await publisherQueue?.value
    }

    @MainActor func testAuthorizationFailureDiscardsAnOfferReceivedDuringSetup() async throws {
        let requested = expectation(description: "ICE request is pending")
        let lock = NSLock()
        var pending: RemoteHTTPFixture?
        let service = try service { request in lock.withLock { pending = request }; requested.fulfill() }
        let viewer = viewer(), socket = ViewerSocket(), publisher = try RemotePeer(publishing: true, ice: [])
        defer { viewer.close(); publisher.close(); service.close() }
        viewer.makeSignaling = { _ in socket }
        publisher.onSignal = { socket.onMessage(.init(type: "signal", signal: $0)) }
        let selected = try hand("original")
        let connection = Task { await viewer.connect(service: service, hand: selected) }
        await fulfillment(of: [requested], timeout: 2)
        try await publisher.offer()
        lock.withLock { pending }?.respond(403)
        await connection.value
        XCTAssertEqual(viewer.status, RemoteError.unauthorized.localizedDescription)
        XCTAssertTrue(socket.closed)
        XCTAssertFalse(viewer.connecting)
        XCTAssertFalse(viewer.connected)
        XCTAssertNil(viewer.track)
        XCTAssertEqual(viewer.diagnosticState, "no peer")
        XCTAssertTrue(socket.messages.isEmpty, "Rejected credentials must never produce an answer or input")
    }

    @MainActor func testPendingCredentialSignalingQueueIsBounded() async throws {
        let requested = expectation(description: "ICE request is pending")
        let service = try service { _ in requested.fulfill() }
        let viewer = viewer(), socket = ViewerSocket()
        defer { viewer.close(); service.close() }
        viewer.makeSignaling = { _ in socket }
        let selected = try hand("original")
        let connection = Task { await viewer.connect(service: service, hand: selected) }
        await fulfillment(of: [requested], timeout: 2)
        for _ in 0...128 {
            socket.onMessage(.init(type: "signal", signal: .init(type: .offer, sdp: "pending authorization")))
        }
        await connection.value
        XCTAssertEqual(viewer.status, RemoteError.invalidMessage.localizedDescription)
        XCTAssertTrue(socket.closed)
        XCTAssertFalse(viewer.connecting)
        XCTAssertTrue(socket.messages.isEmpty)
    }

    @MainActor func testCancellingCallerCancelsSetupWithoutRetryingQueuedOffers() async throws {
        let requested = expectation(description: "ICE request is pending")
        let unexpected = expectation(description: "Cancelled setup must not retry")
        unexpected.isInverted = true
        let service = try service { request in
            if request.request.url?.path.hasSuffix("/screens") == true { unexpected.fulfill() }
            else { requested.fulfill() }
        }
        let viewer = viewer(), socket = ViewerSocket()
        defer { viewer.close(); service.close() }
        viewer.makeSignaling = { _ in socket }
        let selected = try hand("original")
        let connection = Task { await viewer.connect(service: service, hand: selected) }
        await fulfillment(of: [requested], timeout: 2)
        socket.onMessage(.init(type: "signal", signal: .init(type: .offer, sdp: "pending authorization")))
        connection.cancel()
        await connection.value
        await fulfillment(of: [unexpected], timeout: 1.1)
        XCTAssertEqual(viewer.status, "Disconnected")
        XCTAssertTrue(socket.closed)
        XCTAssertFalse(viewer.connecting)
        XCTAssertFalse(viewer.connected)
        XCTAssertNil(viewer.track)
        XCTAssertTrue(socket.messages.isEmpty)
    }

    @MainActor func testBackgroundResumeKeepsSelectionAndRefreshesPublication() async throws {
        let catalog = surface("restarted")
        let service = try service { request in
            if request.request.url?.path.hasSuffix("/screens") == true { request.respond(200, ["surfaces": [catalog]]) }
            else { request.respond(401) }
        }
        let viewer = viewer()
        defer { viewer.close(); service.close() }
        await viewer.connect(service: service, hand: try hand("original"))
        XCTAssertEqual(viewer.status, RemoteError.unauthorized.localizedDescription)
        XCTAssertFalse(viewer.connecting, "Authorization failures must not keep retrying")
        viewer.suspend()
        XCTAssertEqual(viewer.hand?.generation, "original")
        XCTAssertEqual(viewer.status, "Paused")
        await viewer.resume()
        XCTAssertEqual(viewer.hand?.generation, "restarted")
        XCTAssertFalse(viewer.controlling, "Resuming must require a new explicit control acquisition")
        XCTAssertFalse(viewer.connected)
        viewer.close()
        await viewer.resume()
        await viewer.reconnect()
        XCTAssertNil(viewer.hand)
        XCTAssertEqual(viewer.status, "Disconnected")
    }

    @MainActor func testCloseFencesAnOutstandingConnection() async throws {
        let started = expectation(description: "ICE request started")
        let service = try service { _ in started.fulfill() }
        let viewer = viewer(), socket = ViewerSocket()
        viewer.makeSignaling = { _ in socket }
        defer { viewer.close(); service.close() }
        let hand = try hand("original")
        let connection = Task { await viewer.connect(service: service, hand: hand) }
        await fulfillment(of: [started], timeout: 2)
        XCTAssertTrue(viewer.connecting)
        let staleMessage = socket.onMessage, staleClose = socket.onClose
        viewer.close()
        await connection.value
        staleMessage(.init(type: "signal", signal: .init(type: .offer, sdp: "stale")))
        staleClose(RemoteError.unauthorized)
        XCTAssertTrue(socket.closed)
        XCTAssertNil(viewer.hand)
        XCTAssertNil(viewer.track)
        XCTAssertFalse(viewer.connected)
        XCTAssertFalse(viewer.connecting)
        XCTAssertEqual(viewer.diagnosticState, "no peer")
        XCTAssertEqual(viewer.status, "Disconnected")
    }

    @MainActor func testTransientFailureRetriesTheSameScreenWithFreshGeneration() async throws {
        let retried = expectation(description: "Retry resolved the current publication")
        let catalog = surface("fresh")
        let other = surface("other", machine: "vm:unrelated")
        let lock = NSLock()
        var iceRequests = 0
        let service = try service { request in
            if request.request.url?.path.hasSuffix("/screens") == true {
                request.respond(200, ["surfaces": [other, catalog]])
            } else {
                let count = lock.withLock { iceRequests += 1; return iceRequests }
                request.respond(count == 1 ? 503 : 401)
                if count == 2 { retried.fulfill() }
            }
        }
        let viewer = viewer()
        defer { viewer.close(); service.close() }
        await viewer.connect(service: service, hand: try hand("original"))
        XCTAssertTrue(viewer.connecting)
        XCTAssertEqual(viewer.hand?.machineID, "vm:test")
        await fulfillment(of: [retried], timeout: 3)
        XCTAssertEqual(viewer.hand?.generation, "fresh")
        XCTAssertEqual(viewer.hand?.machineID, "vm:test")
        XCTAssertFalse(viewer.controlling)
    }

    @MainActor func testSuspendingPreventsScheduledRetries() async throws {
        let unexpected = expectation(description: "No background reconnect")
        unexpected.isInverted = true
        let service = try service { request in
            if request.request.url?.path.hasSuffix("/screens") == true { unexpected.fulfill() }
            request.respond(503)
        }
        let viewer = viewer()
        defer { viewer.close(); service.close() }
        await viewer.connect(service: service, hand: try hand("original"))
        viewer.suspend()
        await fulfillment(of: [unexpected], timeout: 1.2)
        XCTAssertEqual(viewer.status, "Paused")
        XCTAssertFalse(viewer.connecting)
        XCTAssertNotNil(viewer.hand)
    }

    @MainActor func testRecoveryWaitsForASlowerVMRestart() async throws {
        let recovered = expectation(description: "Fourth catalog retry finds the restarted VM")
        let catalog = surface("after-restart")
        let lock = NSLock()
        var listings = 0
        var iceRequests = 0
        let service = try service { request in
            if request.request.url?.path.hasSuffix("/screens") == true {
                let count = lock.withLock { listings += 1; return listings }
                request.respond(200, ["surfaces": count < 4 ? [] : [catalog]])
            } else {
                let count = lock.withLock { iceRequests += 1; return iceRequests }
                request.respond(count == 1 ? 503 : 401)
                if count == 2 { recovered.fulfill() }
            }
        }
        let viewer = viewer()
        defer { viewer.close(); service.close() }
        await viewer.connect(service: service, hand: try hand("before-restart"))
        await fulfillment(of: [recovered], timeout: 20)
        XCTAssertEqual(viewer.hand?.generation, "after-restart")
        XCTAssertFalse(viewer.controlling)
    }

    @MainActor func testRecoveryDeadlineStopsRetryingWithoutLosingSelection() async throws {
        let unexpected = expectation(description: "No request after the recovery deadline")
        unexpected.isInverted = true
        let service = try service { request in
            if request.request.url?.path.hasSuffix("/screens") == true { unexpected.fulfill() }
            request.respond(503)
        }
        let viewer = viewer(recoveryWindow: .milliseconds(100))
        defer { viewer.close(); service.close() }
        await viewer.connect(service: service, hand: try hand("original"))
        await fulfillment(of: [unexpected], timeout: 0.3)
        XCTAssertFalse(viewer.connecting)
        XCTAssertEqual(viewer.hand?.generation, "original")
        XCTAssertEqual(viewer.status, RemoteError.unavailable.localizedDescription)
    }
}
