#if os(macOS)
import XCTest
import WebRTC
@testable import NanocodexRemote

private final class PhoneFrames: NSObject, RTCVideoRenderer, @unchecked Sendable {
    let received: XCTestExpectation
    private let lock = NSLock()
    private var count = 0
    init(_ received: XCTestExpectation) { self.received = received }
    func setSize(_ size: CGSize) {}
    func renderFrame(_ frame: RTCVideoFrame?) {
        guard let frame, frame.width >= 200, frame.height >= 400 else { return }
        lock.lock(); defer { lock.unlock() }
        count += 1
        if count == 6 { received.fulfill() }
    }
}

final class PhoneCaptureTests: XCTestCase {
    @MainActor func testPairedPhoneVideoAndHomeInputThroughWebRTC() async throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_TEST_PAIRED_PHONE"] == "1" else {
            throw XCTSkip("Requires the paired physical iPhone runner and loopback device tunnel")
        }
        let input = try await PhoneInput.connect(port: 18100)
        let publisher = try RemotePeer(publishing: true, ice: [])
        let viewer = try RemotePeer(publishing: false, ice: [])
        let screen = try PhoneScreen(source: publisher.videoSource, port: 19100)
        defer { publisher.close(); viewer.close(); input.releaseAll() }
        var hostQueue: Task<Void, Never>?, viewerQueue: Task<Void, Never>?
        publisher.onSignal = { signal in
            let prior = viewerQueue
            viewerQueue = Task { await prior?.value; do { try await viewer.receive(signal) } catch { XCTFail("Viewer: \(error)") } }
        }
        viewer.onSignal = { signal in
            let prior = hostQueue
            hostQueue = Task { await prior?.value; do { try await publisher.receive(signal) } catch { XCTFail("Host: \(error)") } }
        }
        let ready = expectation(description: "Phone control transport ready")
        var opened = false
        viewer.onChannelsReady = { if !opened { opened = true; ready.fulfill() } }
        let visible = expectation(description: "Six physical phone frames decoded through WebRTC")
        let frames = PhoneFrames(visible)
        viewer.onVideoTrack = { $0.add(frames) }
        screen.onFailure = { error in XCTFail("Physical screen stream: \(error)") }
        let accepted = expectation(description: "Phone accepted Home input from the viewer data channel")
        publisher.onData = { data, motion in
            do { XCTAssertFalse(motion); try input.apply(RemoteInput.decode(data)); accepted.fulfill() }
            catch { XCTFail("Phone input: \(error)") }
        }
        try await screen.start(); try await publisher.offer()
        await fulfillment(of: [ready, visible], timeout: 20)
        guard opened else { await screen.stop(); return }
        var launch = URLRequest(url: URL(string: "http://127.0.0.1:18100/wda/apps/launchUnattached")!)
        launch.httpMethod = "POST"; launch.setValue("application/json", forHTTPHeaderField: "Content-Type")
        launch.httpBody = Data(#"{"bundleId":"com.apple.calculator"}"#.utf8)
        let (_, launchResponse) = try await URLSession.shared.data(for: launch)
        XCTAssertEqual((launchResponse as? HTTPURLResponse)?.statusCode, 200)
        let event = RemoteInput(kind: .key, sequence: 1, generation: "physical-phone-evidence", down: true, key: 74)
        try viewer.send(JSONEncoder().encode(event))
        await fulfillment(of: [accepted], timeout: 5)
        var homeVisible = false
        let deadline = ProcessInfo.processInfo.systemUptime + 5
        while ProcessInfo.processInfo.systemUptime < deadline {
            let (data, _) = try await URLSession.shared.data(from: URL(string: "http://127.0.0.1:18100/wda/activeAppInfo")!)
            let value = (try JSONSerialization.jsonObject(with: data) as? [String: Any])?["value"] as? [String: Any]
            if value?["bundleId"] as? String == "com.apple.springboard" { homeVisible = true; break }
            try await Task.sleep(for: .milliseconds(100))
        }
        XCTAssertTrue(homeVisible, "Physical iPhone returned from Calculator to its Home Screen")
        viewer.remoteVideoTrack?.remove(frames); await screen.stop()
        await hostQueue?.value; await viewerQueue?.value
    }
}
#endif
