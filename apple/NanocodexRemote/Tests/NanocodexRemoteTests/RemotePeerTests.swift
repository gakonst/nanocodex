import XCTest
import CoreVideo
import WebRTC
@testable import NanocodexRemote

private final class FrameReceiver: NSObject, RTCVideoRenderer, @unchecked Sendable {
    let received: XCTestExpectation
    private let lock = NSLock()
    private var fulfilled = false
    init(_ received: XCTestExpectation) { self.received = received }
    func setSize(_ size: CGSize) {}
    func renderFrame(_ frame: RTCVideoFrame?) {
        guard let frame, frame.width == 320, frame.height == 240 else { return }
        lock.lock(); defer { lock.unlock() }
        if !fulfilled { fulfilled = true; received.fulfill() }
    }
}

final class RemotePeerTests: XCTestCase {
    @MainActor func testRealWebRTCVideoAndBidirectionalInputChannels() async throws {
        struct RelayConfiguration: Decodable { let initial: [RemoteICE]; let renewed: [RemoteICE] }
        let relay: RelayConfiguration?
        if let path = ProcessInfo.processInfo.environment["NANOCODEX_TEST_TURN_CONFIG"] {
            relay = try JSONDecoder().decode(RelayConfiguration.self, from: Data(contentsOf: URL(fileURLWithPath: path)))
            // Explicit private test configurations may use the local Coturn
            // fixture or the production Cloudflare relay. Never log credentials.
            let allowed = ["turn:127.0.0.1:", "turn:turn.cloudflare.com:", "turns:turn.cloudflare.com:"]
            for servers in [relay!.initial, relay!.renewed] {
                XCTAssertFalse(servers.isEmpty)
                for server in servers {
                    XCTAssertFalse(server.urls.isEmpty)
                    XCTAssertTrue(server.urls.allSatisfy { url in allowed.contains { url.hasPrefix($0) } })
                }
            }
        } else { relay = nil }
        let publisher = try RemotePeer(publishing: true, ice: relay?.initial ?? [], relayOnly: relay != nil)
        let viewer = try RemotePeer(publishing: false, ice: relay?.initial ?? [], relayOnly: relay != nil)
        defer { publisher.close(); viewer.close() }
        var offeredICE: [String] = []
        var answers = 0
        let renegotiated = expectation(description: "ICE restart answer applied")
        // Serialize signaling exactly as the service does, including early ICE.
        var publisherQueue: Task<Void, Never>?, viewerQueue: Task<Void, Never>?
        publisher.onSignal = { signal in
            if signal.type == .offer, let sdp = signal.sdp {
                offeredICE.append(sdp.components(separatedBy: "\r\n").first(where: { $0.hasPrefix("a=ice-ufrag:") }) ?? "missing")
            }
            let previous = viewerQueue
            viewerQueue = Task { await previous?.value; do { try await viewer.receive(signal) } catch { XCTFail("Viewer: \(error)") } }
        }
        viewer.onSignal = { signal in
            let previous = publisherQueue
            publisherQueue = Task {
                await previous?.value
                do {
                    try await publisher.receive(signal)
                    if signal.type == .answer { answers += 1; if answers == 2 { renegotiated.fulfill() } }
                } catch { XCTFail("Publisher: \(error)") }
            }
        }
        let channels = expectation(description: "Viewer control channels opened")
        var opened = false
        viewer.onChannelsReady = { if !opened { opened = true; channels.fulfill() } }
        let rendered = expectation(description: "Encoded video decoded at viewer")
        let renderer = FrameReceiver(rendered)
        let firstDecodedFrame = expectation(description: "Diagnostics report exactly one decoded frame")
        firstDecodedFrame.assertForOverFulfill = true
        let probe = RemoteFirstFrameProbe { time, width, height in
            XCTAssertGreaterThan(time, 0)
            XCTAssertEqual(width, 320); XCTAssertEqual(height, 240)
            firstDecodedFrame.fulfill()
        }
        viewer.onVideoTrack = { $0.add(renderer); $0.add(probe) }
        let control = expectation(description: "Reliable input reaches publisher")
        let motion = expectation(description: "Disposable motion reaches publisher")
        let reply = expectation(description: "Control acknowledgement reaches viewer")
        publisher.onData = { data, isMotion in
            if isMotion { XCTAssertEqual(data, Data("move".utf8)); motion.fulfill() }
            else { XCTAssertEqual(data, Data("key-up".utf8)); control.fulfill(); _ = try? publisher.send(Data("granted".utf8)) }
        }
        viewer.onData = { data, isMotion in XCTAssertFalse(isMotion); XCTAssertEqual(data, Data("granted".utf8)); reply.fulfill() }
        try await publisher.offer()
        await fulfillment(of: [channels], timeout: 15)
        guard opened else { return }
        try viewer.send(Data("key-up".utf8)); try viewer.send(Data("move".utf8), motion: true)
        var pixelBuffer: CVPixelBuffer?
        XCTAssertEqual(CVPixelBufferCreate(kCFAllocatorDefault, 320, 240, kCVPixelFormatType_32BGRA,
            [kCVPixelBufferIOSurfacePropertiesKey: [:]] as CFDictionary, &pixelBuffer), kCVReturnSuccess)
        let buffer = try XCTUnwrap(pixelBuffer)
        CVPixelBufferLockBaseAddress(buffer, [])
        memset(CVPixelBufferGetBaseAddress(buffer), 96, CVPixelBufferGetDataSize(buffer))
        CVPixelBufferUnlockBaseAddress(buffer, [])
        let capturer = RTCVideoCapturer(delegate: publisher.videoSource)
        let frames = Task {
            for _ in 0..<600 {
                guard !Task.isCancelled else { return }
                let timestamp = Int64(ProcessInfo.processInfo.systemUptime * 1_000_000_000)
                publisher.videoSource.capturer(capturer, didCapture: RTCVideoFrame(buffer: RTCCVPixelBuffer(pixelBuffer: buffer), rotation: ._0, timeStampNs: timestamp))
                try? await Task.sleep(for: .milliseconds(33))
            }
        }
        await fulfillment(of: [control, motion, reply, rendered, firstDecodedFrame], timeout: 10)
        let hold: [RemoteInput] = [
            .init(kind: .button, sequence: 1, generation: "capture", x: 0.5, y: 0.5, button: 1, down: true),
            .init(kind: .relativeMove, sequence: 2, generation: "capture", deltaX: 24, deltaY: -12),
            .init(kind: .button, sequence: 3, generation: "capture", button: 1, down: false),
        ]
        let heldInput = expectation(description: "Captured right-button movement arrives reliably in order")
        heldInput.expectedFulfillmentCount = hold.count
        var receivedHold: [RemoteInput] = []
        publisher.onData = { data, isMotion in
            XCTAssertFalse(isMotion, "Relative displacements cannot use the lossy motion channel")
            do { receivedHold.append(try RemoteInput.decode(data)); heldInput.fulfill() }
            catch { XCTFail("Invalid captured input: \(error)") }
        }
        for event in hold { try viewer.send(JSONEncoder().encode(event)) }
        await fulfillment(of: [heldInput], timeout: 5)
        XCTAssertEqual(receivedHold, hold)
        let originalCandidate = await publisher.selectedLocalCandidate()
        if relay != nil { XCTAssertTrue(originalCandidate?.hasPrefix("relay:") == true) }
        let restartedInput = expectation(description: "Existing input channel survives ICE restart")
        publisher.onData = { data, _ in XCTAssertEqual(data, Data("after-restart".utf8)); restartedInput.fulfill() }
        try viewer.updateICE(relay?.renewed ?? [])
        try await publisher.restartICE(relay?.renewed ?? [])
        await fulfillment(of: [renegotiated], timeout: 10)
        XCTAssertEqual(offeredICE.count, 2)
        XCTAssertEqual(Set(offeredICE).count, 2, "ICE restart must replace the ICE username fragment")
        if relay != nil {
            let deadline = ProcessInfo.processInfo.systemUptime + 5
            var renewedCandidate = await publisher.selectedLocalCandidate()
            while renewedCandidate == originalCandidate && ProcessInfo.processInfo.systemUptime < deadline {
                try await Task.sleep(for: .milliseconds(50)); renewedCandidate = await publisher.selectedLocalCandidate()
            }
            XCTAssertTrue(renewedCandidate?.hasPrefix("relay:") == true)
            XCTAssertNotEqual(renewedCandidate, originalCandidate, "Renewal must select a new TURN allocation")
        }
        try viewer.send(Data("after-restart".utf8))
        viewer.remoteVideoTrack?.remove(renderer)
        let renewedVideo = expectation(description: "Decoded video continues after ICE restart")
        let renewedRenderer = FrameReceiver(renewedVideo)
        viewer.remoteVideoTrack?.add(renewedRenderer)
        await fulfillment(of: [restartedInput, renewedVideo], timeout: 10)
        frames.cancel()
        viewer.remoteVideoTrack?.remove(renewedRenderer)
        await publisherQueue?.value; await viewerQueue?.value
    }
}

extension RemotePeerTests {
    @MainActor func testAudioRequiresNegotiatedMicrophoneAndCloseRevokesState() async throws {
        let peer = try RemotePeer(publishing: false, ice: [])
        XCTAssertFalse(peer.microphoneEnabled)
        XCTAssertTrue(peer.speakersEnabled)
        // Rejection occurs before any OS permission request or capture device.
        do {
            try await peer.setMicrophoneEnabled(true)
            XCTFail("A viewer without a negotiated return audio sender cannot enable microphone")
        } catch {
            XCTAssertFalse(peer.microphoneEnabled)
        }
        peer.setSpeakersEnabled(false)
        XCTAssertFalse(peer.speakersEnabled)
        try await peer.setMicrophoneEnabled(false)
        peer.close()
        peer.stopMicrophone()
        XCTAssertFalse(peer.microphoneEnabled)
        do {
            try await peer.setMicrophoneEnabled(true)
            XCTFail("Closed peer cannot request microphone access")
        } catch { XCTAssertFalse(peer.microphoneEnabled) }
    }
}

// These exercise lifecycle state without opening a capture device.
extension RemotePeerTests {
    @MainActor func testAudioSessionStopNotifiesViewerWithoutClosingTransport() throws {
        let peer = try RemotePeer(publishing: false, ice: [])
        defer { peer.close() }
        var stops = 0
        var transportChanges = 0
        peer.onMicrophoneStopped = { stops += 1; XCTAssertFalse(peer.microphoneEnabled) }
        peer.onState = { _ in transportChanges += 1 }
        peer.audioSessionStoppedMicrophone()
        XCTAssertEqual(stops, 1, "Notify even before capture starts to cancel pending viewer opt-in")
        XCTAssertEqual(transportChanges, 0, "An audio interruption is not a transport failure")
        XCTAssertTrue(peer.speakersEnabled)
        peer.close()
        peer.audioSessionStoppedMicrophone()
        XCTAssertEqual(stops, 1, "Queued OS notifications cannot mutate a closed viewer")
    }

    @MainActor func testAudioSessionStopDoesNotAffectPublisher() throws {
        let peer = try RemotePeer(publishing: true, ice: [])
        defer { peer.close() }
        peer.onMicrophoneStopped = { XCTFail("Publisher does not own viewer microphone") }
        peer.audioSessionStoppedMicrophone()
        XCTAssertFalse(peer.microphoneEnabled)
    }
}
