import Foundation
import XCTest
@testable import NanocodexRemote

private final class PlaybackStub: URLProtocol {
    nonisolated(unsafe) static var responses: [(Int, String)] = []
    nonisolated(unsafe) static var requests: [URLRequest] = []
    nonisolated(unsafe) static var bodies: [Data] = []
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        Self.requests.append(request)
        if let stream = request.httpBodyStream {
            stream.open(); var data = Data(); var buffer = [UInt8](repeating: 0, count: 4096)
            while stream.hasBytesAvailable { let n = stream.read(&buffer, maxLength: buffer.count); if n <= 0 { break }; data.append(buffer, count: n) }
            stream.close(); Self.bodies.append(data)
        } else { Self.bodies.append(request.httpBody ?? Data()) }
        let (status, body) = Self.responses.removeFirst()
        client?.urlProtocol(self, didReceive: HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil, headerFields: ["content-type": "application/json"])!, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Data(body.utf8)); client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() {}
}

final class RemotePlaybackLinkTests: XCTestCase {
    private let link = #""id":"pl_1","operation_id":"x","machine_id":"m","surface_id":"s","preset":"720p","state":"starting","created_at":1700000000000,"expires_at":1700003600000"#

    private func service() throws -> RemoteService {
        PlaybackStub.requests = []; PlaybackStub.bodies = []
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [PlaybackStub.self]
        return try RemoteService(origin: URL(string: "https://example.test")!, configuration: configuration) { _ in }
    }

    private func hand(playback: Bool?) throws -> RemoteHand {
        let extra = playback.map { #","playback":\#($0)"# } ?? ""
        return try JSONDecoder().decode(RemoteHand.self, from: Data(#"{"id":"s","name":"Desktop","kind":"desktop","width":1,"height":1,"controllable":true,"machine_id":"m","machine_name":"Mac","generation":"g1"\#(extra)}"#.utf8))
    }

    func testCatalogPlaybackCapabilityIsOptional() throws {
        XCTAssertNil(try hand(playback: nil).playback)
        XCTAssertEqual(try hand(playback: true).playback, true)
    }

    func testCreateSendsOperationAndShowsSameOriginURLOnce() async throws {
        let service = try service(), id = UUID()
        PlaybackStub.responses = [(201, "{\(link),\"url\":\"https://example.test/v1/screen-playback/pl_1/tok/index.m3u8\"}")]
        let receipt = try await service.createPlaybackLink(hand: hand(playback: true), operationID: id, expiresInSeconds: 900, preset: .p1080)
        XCTAssertEqual(receipt.url?.absoluteString, "https://example.test/v1/screen-playback/pl_1/tok/index.m3u8")
        XCTAssertEqual(PlaybackStub.requests.first?.httpMethod, "POST")
        XCTAssertEqual(PlaybackStub.requests.first?.url?.path, "/v1/account/hands/playback-links")
        let body = try JSONSerialization.jsonObject(with: PlaybackStub.bodies[0]) as! [String: Any]
        XCTAssertEqual(body["operation_id"] as? String, id.uuidString.lowercased())
        XCTAssertEqual(body["generation"] as? String, "g1"); XCTAssertEqual(body["preset"] as? String, "1080p")
        XCTAssertEqual(body["expires_in_seconds"] as? Int, 900)

        PlaybackStub.responses = [(200, "{\(link),\"url_available\":false}")]
        let replay = try await service.createPlaybackLink(hand: hand(playback: true), operationID: id, expiresInSeconds: 900, preset: .p1080)
        XCTAssertNil(replay.url); XCTAssertEqual(replay.link.id, "pl_1")
    }

    func testForeignURLAndServerFailureAreUncertainWithoutRetry() async throws {
        let service = try service()
        PlaybackStub.responses = [(201, "{\(link),\"url\":\"https://evil.test/v1/screen-playback/pl_1/tok/index.m3u8\"}")]
        do { _ = try await service.createPlaybackLink(hand: hand(playback: true), operationID: UUID(), expiresInSeconds: 3600, preset: .p720); XCTFail() }
        catch let error as RemotePlaybackError { XCTAssertTrue(error.uncertain) }
        PlaybackStub.responses = [(502, "")]
        do { _ = try await service.createPlaybackLink(hand: hand(playback: true), operationID: UUID(), expiresInSeconds: 3600, preset: .p720); XCTFail() }
        catch let error as RemotePlaybackError { XCTAssertTrue(error.uncertain) }
        XCTAssertEqual(PlaybackStub.requests.count, 2, "no automatic POST retry")
        PlaybackStub.responses = [(409, #"{"error":"too_many_streams"}"#)]
        do { _ = try await service.createPlaybackLink(hand: hand(playback: true), operationID: UUID(), expiresInSeconds: 3600, preset: .p720); XCTFail() }
        catch let error as RemotePlaybackError { XCTAssertFalse(error.uncertain); XCTAssertEqual(error.code, "too_many_streams") }
    }

    func testOldHandNeverPostsAndRevokeIsIdempotent() async throws {
        let service = try service()
        do { _ = try await service.createPlaybackLink(hand: hand(playback: nil), operationID: UUID(), expiresInSeconds: 3600, preset: .p720); XCTFail() }
        catch let error as RemotePlaybackError { XCTAssertEqual(error.code, "unsupported") }
        XCTAssertTrue(PlaybackStub.requests.isEmpty)
        PlaybackStub.responses = [(404, #"{"error":"not_found"}"#), (200, "{\"data\":[{\(link)}]}")]
        try await service.revokePlaybackLink(id: "pl_1")
        XCTAssertEqual(PlaybackStub.requests.first?.httpMethod, "DELETE")
        XCTAssertEqual(PlaybackStub.requests.first?.url?.path, "/v1/account/hands/playback-links/pl_1")
        let links = try await service.playbackLinks()
        XCTAssertEqual(links.map(\.id), ["pl_1"]); XCTAssertTrue(links[0].isActive)
    }
}
