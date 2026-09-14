#if os(macOS)
import AppKit
import XCTest
import WebRTC
@testable import NanocodexRemote

private final class MacFrameReceiver: NSObject, RTCVideoRenderer, @unchecked Sendable {
    private let lock = NSLock()
    private var dimensions: (Int32, Int32)?
    var receivedScreen: Bool {
        lock.lock(); defer { lock.unlock() }
        guard let (width, height) = dimensions else { return false }
        return width >= 320 && height >= 240
    }
    func setSize(_ size: CGSize) {}
    func renderFrame(_ frame: RTCVideoFrame?) {
        guard let frame else { return }
        lock.lock(); dimensions = (frame.width, frame.height); lock.unlock()
    }
}

final class AccountMacTests: XCTestCase {
    // Start sharing from the Mac app's real Screens UI, then focus its
    // empty composer before running this test. Inspect that composer afterwards
    // to confirm the marker arrived; this test does not claim to inspect app UI.
    @MainActor func testPublishedMacVideoAndControlSession() async throws {
        let environment = ProcessInfo.processInfo.environment
        guard let machineID = environment["NANOCODEX_TEST_MAC_MACHINE_ID"] else {
            throw XCTSkip("Requires an explicitly selected Mac shared through its native UI")
        }
        var values: [String: String] = [:]
        if let path = environment["NANOCODEX_TEST_REMOTE_ENV"] {
            for line in try String(contentsOfFile: path, encoding: .utf8).split(separator: "\n") {
                guard let split = line.firstIndex(of: "=") else { continue }
                values[String(line[..<split])] = String(line[line.index(after: split)...])
            }
        }
        let origin = try XCTUnwrap(URL(string: try XCTUnwrap(environment["NANOCODEX_MANAGED_URL"] ?? values["NANOCODEX_MANAGED_URL"])))
        let local = ["127.0.0.1", "localhost"].contains(origin.host ?? "")
        let live = environment["NANOCODEX_TEST_MAC_LIVE"] == "1" && origin.scheme == "https"
        guard local || live else {
            throw XCTSkip("A live account requires NANOCODEX_TEST_MAC_LIVE=1")
        }
        let token = try XCTUnwrap(environment["NANOCODEX_API_KEY"] ?? values["NANOCODEX_API_KEY"])
        let service = try RemoteService(origin: origin) { $0.setValue("Bearer " + token, forHTTPHeaderField: "Authorization") }
        let viewer = RemoteViewer()
        defer { viewer.close(); service.close() }
        let hands = try await service.list()
        let hand = try XCTUnwrap(hands.first { $0.machineID == machineID && $0.kind == .desktop })
        XCTAssertTrue(hand.controllable)
        await viewer.connect(service: service, hand: hand)
        try await eventually { viewer.connected && viewer.track != nil }
        let track = try XCTUnwrap(viewer.track), frames = MacFrameReceiver()
        track.add(frames)
        defer { track.remove(frames) }
        try await eventually { frames.receivedScreen }
        viewer.takeControl()
        try await eventually { viewer.controlling }

        // Never type into whichever unrelated application happens to be frontmost.
        let targetBundle = live ? "xyz.paradigm.nanocodex.macos" : "xyz.paradigm.nanocodex.macos.remote-evidence"
        guard NSWorkspace.shared.frontmostApplication?.bundleIdentifier == targetBundle else {
            throw XCTSkip("Focus the selected Mac app's empty composer before sending the marker")
        }
        viewer.input(kind: .text, text: "WebRTC Mac input verifiedx")
        for down in [true, false] { viewer.input(kind: .key, down: down, key: 42) }
        try await Task.sleep(for: .milliseconds(500))
        XCTAssertTrue(viewer.connected && viewer.controlling)
        viewer.releaseControl()
        XCTAssertFalse(viewer.controlling)
    }

    @MainActor private func eventually(_ predicate: () -> Bool) async throws {
        let deadline = ProcessInfo.processInfo.systemUptime + 15
        while ProcessInfo.processInfo.systemUptime < deadline {
            if predicate() { return }
            try await Task.sleep(for: .milliseconds(100))
        }
        XCTFail("The published Mac did not reach the expected video/control state")
        throw RemoteError.unavailable
    }
}
#endif
