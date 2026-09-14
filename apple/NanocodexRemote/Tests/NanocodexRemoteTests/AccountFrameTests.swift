#if os(macOS)
import XCTest
@testable import NanocodexRemote

final class AccountFrameTests: XCTestCase {
    // Opt-in local SDK fixture only. The fixture verifies the file written via
    // viewer input, so a signaling acknowledgement cannot satisfy the test.
    @MainActor func testCloudflareFramesAndTerminalInput() async throws {
        guard let address = ProcessInfo.processInfo.environment["NANOCODEX_TEST_FRAME_ORIGIN"],
              let origin = URL(string: address), origin.host == "127.0.0.1" else {
            throw XCTSkip("Requires the explicitly started local Cloudflare desktop fixture")
        }
        let service = try RemoteService(origin: origin) {
            $0.setValue("Bearer local-desktop-fixture", forHTTPHeaderField: "Authorization")
        }
        let viewer = RemoteViewer()
        defer { viewer.close(); service.close() }
        let hands = try await service.list()
        let hand = try XCTUnwrap(hands.first { $0.transport == .frames })
        await viewer.connect(service: service, hand: hand)
        try await eventually { viewer.connected && viewer.frame != nil }
        XCTAssertNil(viewer.track)
        viewer.takeControl(); try await eventually { viewer.controlling }
        viewer.input(kind: .button, x: 0.5, y: 0.5, button: 0, down: true)
        viewer.input(kind: .button, x: 0.5, y: 0.5, button: 0, down: false)
        viewer.input(kind: .text, text: "printf native-frame-input > native-frame-marker")
        for down in [true, false] { viewer.input(kind: .key, down: down, key: 40) }
        var verified = false
        for _ in 0..<30 {
            var request = URLRequest(url: origin.appendingPathComponent("verify"))
            request.setValue("Bearer local-desktop-fixture", forHTTPHeaderField: "Authorization")
            let (data, _) = try await URLSession.shared.data(for: request)
            if String(data: data, encoding: .utf8) == "native-frame-input" { verified = true; break }
            try await Task.sleep(for: .milliseconds(100))
        }
        XCTAssertTrue(verified)
        viewer.suspend()
        XCTAssertNil(viewer.frame); XCTAssertFalse(viewer.controlling)
        XCTAssertEqual(viewer.hand?.machineID, hand.machineID)
        await viewer.resume()
        try await eventually { viewer.connected && viewer.frame != nil }
        XCTAssertFalse(viewer.controlling)
    }

    @MainActor private func eventually(_ ready: () -> Bool) async throws {
        for _ in 0..<150 {
            if ready() { return }
            try await Task.sleep(for: .milliseconds(100))
        }
        XCTFail("Frame viewer did not become ready")
        throw RemoteError.unavailable
    }
}
#endif
