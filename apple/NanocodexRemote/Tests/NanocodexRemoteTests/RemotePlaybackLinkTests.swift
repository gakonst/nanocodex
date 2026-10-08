import Foundation
import XCTest
@testable import NanocodexRemote

/// Runs against the real workerd playback journey fixture, using a synthetic
/// account and an actual published Hand socket. No URLProtocol interception.
final class RemotePlaybackLinkTests: XCTestCase {
    func testPlaybackHTTPJourney() async throws {
        let env = ProcessInfo.processInfo.environment
        guard let value = env["SCREEN_PLAYBACK_TEST_ORIGIN"], let origin = URL(string: value),
              let token = env["SCREEN_PLAYBACK_TEST_TOKEN"] else {
            throw XCTSkip("Run from the screen playback workerd journey with its synthetic account.")
        }
        let service = try RemoteService(origin: origin) { request in
            request.setValue("Bearer " + token, forHTTPHeaderField: "Authorization")
        }
        defer { service.close() }
        let hands = try await service.list()
        let hand = try XCTUnwrap(hands.first(where: { $0.playback == true }))
        let operation = UUID()
        let created = try await service.createPlaybackLink(hand: hand, operationID: operation,
            expiresInSeconds: 60, preset: .p720)
        let url = try XCTUnwrap(created.url)
        do {
            XCTAssertEqual(created.link.machineID, hand.machineID)
            XCTAssertEqual(created.link.surfaceID, hand.id)
            XCTAssertTrue(created.link.isActive)
            let replay = try await service.createPlaybackLink(hand: hand, operationID: operation,
                expiresInSeconds: 60, preset: .p720)
            XCTAssertEqual(replay.link.id, created.link.id)
            XCTAssertNil(replay.url)
            let listed = try await service.playbackLinks()
            XCTAssertTrue(listed.contains(where: { $0.id == created.link.id }))
            let (_, response) = try await URLSession.shared.data(from: url)
            XCTAssertTrue([200, 503].contains((response as? HTTPURLResponse)?.statusCode ?? 0))
            try await service.revokePlaybackLink(id: created.link.id)
            try await service.revokePlaybackLink(id: created.link.id)
            let (_, revoked) = try await URLSession.shared.data(from: url)
            XCTAssertEqual((revoked as? HTTPURLResponse)?.statusCode, 404)
            let after = try await service.playbackLinks()
            XCTAssertEqual(after.first(where: { $0.id == created.link.id })?.state, .revoked)
        } catch {
            try? await service.revokePlaybackLink(id: created.link.id)
            throw error
        }
    }
}
