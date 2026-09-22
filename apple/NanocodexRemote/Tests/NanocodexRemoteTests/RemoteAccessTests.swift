import Foundation
import XCTest
import InboxCore
@testable import NanocodexRemote

private final class RejectedScreenSnapshot: URLProtocol {
    override class func canInit(with request: URLRequest) -> Bool { request.url?.path == "/v1/account/hands/screens" }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        let response = HTTPURLResponse(url: request.url!, statusCode: 200, httpVersion: "HTTP/1.1", headerFields: [
            "Content-Type": "application/json", "x-nanocodex-access": "ncx_access_v1.invalid.signature",
            "x-nanocodex-access-ttl-ms": "120000",
        ])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Data("{\"surfaces\":[]}".utf8))
        client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() {}
}

final class RemoteAccessTests: XCTestCase {
    /// A selected remote host supplies actual WAN video; no control is acquired.
    @MainActor func testLivePublishedScreenLatency() async throws {
        let env = ProcessInfo.processInfo.environment
        guard env["NANOCODEX_TEST_REMOTE_ACCESS"] == "1", let machine = env["NANOCODEX_TEST_REMOTE_MACHINE_ID"],
              let address = env["NANOCODEX_MANAGED_URL"], let origin = URL(string: address), origin.scheme == "https",
              let key = env["NANOCODEX_API_KEY"], let output = env["NANOCODEX_TEST_REMOTE_LATENCY_OUTPUT"],
              env["NANOCODEX_REMOTE_DIAGNOSTICS"] == "1" else {
            throw XCTSkip("Requires an explicitly selected live screen latency fixture")
        }
        let service = try RemoteService(origin: origin) { $0.setValue("Bearer " + key, forHTTPHeaderField: "Authorization") }
        let viewer = RemoteViewer()
        defer { viewer.close(); service.close() }
        let began = ProcessInfo.processInfo.systemUptime
        let hands = try await service.list()
        let hand = try XCTUnwrap(hands.first { $0.machineID == machine })
        let catalogMs = (ProcessInfo.processInfo.systemUptime - began) * 1000
        print("REMOTE_WAN_TRANSPORT \(hand.transport?.rawValue ?? "webrtc") \(hand.width)x\(hand.height)")
        var samples: [[String: Any]] = []
        for _ in 0..<3 {
            await viewer.connect(service: service, hand: hand)
            let deadline = ContinuousClock.now + .seconds(30)
            var sample: [String: Any]?
            while ContinuousClock.now < deadline {
                if let data = viewer.diagnosticPresentation.data(using: .utf8),
                   let value = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                   let frame = value["first_frame"] as? [String: Int],
                   frame["width", default: 0] > 0, frame["height", default: 0] > 0 {
                    sample = value; break
                }
                try await Task.sleep(for: .milliseconds(25))
            }
            XCTAssertFalse(viewer.controlling)
            if sample == nil {
                print("REMOTE_WAN_FAILURE " + viewer.diagnosticPresentation)
                print("REMOTE_WAN_ICE " + (await viewer.diagnosticICE(includeAddresses: false)))
            }
            samples.append(try XCTUnwrap(sample, "The selected remote host must deliver a decoded frame"))
            viewer.close()
        }
        try JSONSerialization.data(withJSONObject: ["catalog_ms": catalogMs, "samples": samples], options: [.prettyPrinted, .sortedKeys])
            .write(to: URL(fileURLWithPath: output), options: .atomic)
    }

    // Inject only an invalid cached snapshot; the actual viewer socket connects
    // to the selected live account and must recover before screen admission.
    // This checks URLSession's real HTTP rejection metadata and sends no input.
    @MainActor func testLiveRejectedViewerSnapshotRetriesBeforeAdmission() async throws {
        let env = ProcessInfo.processInfo.environment
        guard env["NANOCODEX_TEST_REMOTE_ACCESS"] == "1", let machine = env["NANOCODEX_TEST_REMOTE_MACHINE_ID"],
              let address = env["NANOCODEX_MANAGED_URL"], let origin = URL(string: address),
              origin.scheme == "https", let key = env["NANOCODEX_API_KEY"] else {
            throw XCTSkip("Requires an explicitly selected live screen admission fixture")
        }
        ManagedAccess.clear()
        defer { ManagedAccess.clear() }
        let actual = try RemoteService(origin: origin) { $0.setValue("Bearer " + key, forHTTPHeaderField: "Authorization") }
        defer { actual.close() }
        let hands = try await actual.list()
        let hand = try XCTUnwrap(hands.first { $0.machineID == machine })
        var rejected = URLRequest(url: origin.appendingPathComponent("v1/account/hands/ice"))
        rejected.httpMethod = "POST"
        rejected.setValue("Bearer " + key, forHTTPHeaderField: "Authorization")
        rejected.setValue("ncx_access_v1.invalid.signature", forHTTPHeaderField: "x-nanocodex-access")
        let (_, rejection) = try await URLSession.shared.data(for: rejected)
        XCTAssertEqual((rejection as? HTTPURLResponse)?.statusCode, 401)
        XCTAssertEqual((rejection as? HTTPURLResponse)?.value(forHTTPHeaderField: "x-nanocodex-access-rejected"), "1")
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [RejectedScreenSnapshot.self]
        let service = try RemoteService(origin: origin, configuration: configuration) {
            $0.setValue("Bearer " + key, forHTTPHeaderField: "Authorization")
        }
        defer { service.close() }
        // Ensure our injected response replaces the previous catalog's snapshot.
        ManagedAccess.clear()
        _ = try await service.list()
        let signaling = RemoteSignaling(service: service)
        defer { signaling.close() }
        var ready = false
        var failure: Error?
        signaling.onMessage = { if $0.type == "ready" { ready = true } }
        signaling.onClose = { failure = $0 }
        try signaling.connect(hand: hand)
        let deadline = ContinuousClock.now + .seconds(15)
        while !ready, failure == nil, ContinuousClock.now < deadline { try await Task.sleep(for: .milliseconds(10)) }
        XCTAssertNil(failure)
        XCTAssertTrue(ready, "Rejected authority must retry live before any screen input is admitted")
    }
}
