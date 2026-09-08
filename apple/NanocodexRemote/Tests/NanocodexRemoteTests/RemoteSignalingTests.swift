import Foundation
import XCTest
@testable import NanocodexRemote

final class RemoteSignalingTests: XCTestCase {
    @MainActor func testOnlyExplicitPublisherReplacementIsTerminal() {
        let underlying = URLError(.networkConnectionLost)
        let replaced = Data("Host replaced".utf8)
        XCTAssertEqual(RemoteSignaling.disconnectError(underlying, publishing: true,
            code: 1008, reason: replaced) as? RemoteError, .hostReplaced)
        for (publishing, code, reason) in [
            (false, 1008, Optional(replaced)),
            (true, 1000, Optional(replaced)),
            (true, 1008, Optional(Data("Authorization expired".utf8))),
            (true, 1008, Optional(Data("Remote connection closed".utf8))),
            (true, 1008, nil),
        ] {
            XCTAssertEqual(RemoteSignaling.disconnectError(underlying, publishing: publishing,
                code: code, reason: reason) as? URLError, underlying)
        }
    }

    // The local fixture sends an authenticated-role-equivalent policy close.
    // This checks Foundation's actual close metadata, not a synthetic NSError.
    @MainActor func testLocalWebSocketPreservesPublisherReplacementReason() async throws {
        guard let raw = ProcessInfo.processInfo.environment["NANOCODEX_TEST_REPLACEMENT_ORIGIN"],
              let origin = URL(string: raw), origin.scheme == "http",
              ["127.0.0.1", "localhost"].contains(origin.host ?? "") else {
            throw XCTSkip("Requires the explicitly selected loopback WebSocket fixture")
        }
        let service = try RemoteService(origin: origin) { _ in }
        defer { service.close() }
        for publisher in [true, false] {
            let transport = RemoteSignaling(service: service)
            var failure: Error?
            var closed = false
            transport.onClose = { failure = $0; closed = true }
            let hand = publisher ? nil : try JSONDecoder().decode(RemoteHand.self, from: Data("""
                {"id":"display","name":"Fixture","kind":"desktop","width":640,"height":480,
                 "controllable":true,"machine_id":"fixture","machine_name":"Fixture","generation":"one"}
                """.utf8))
            try transport.connect(hand: hand)
            let deadline = ContinuousClock.now + .seconds(5)
            while !closed, ContinuousClock.now < deadline { try await Task.sleep(for: .milliseconds(10)) }
            XCTAssertTrue(closed)
            if publisher { XCTAssertEqual(failure as? RemoteError, .hostReplaced) }
            else { XCTAssertNotNil(failure); XCTAssertNotEqual(failure as? RemoteError, .hostReplaced) }
            transport.close()
        }
    }
}
