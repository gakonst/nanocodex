import XCTest
import Combine
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

final class RemoteViewerTests: XCTestCase {
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
