import XCTest
@testable import NanocodexRemote

final class RemoteWireDecoderTests: XCTestCase {
    func testActorParserPreservesOrderAndEnforcesByteLimit() async throws {
        let decoder = RemoteWireDecoder()
        let first = try await decoder.decode(#"{"type":"ready","connection_id":"fixture"}"#, limit: 100)
        let second = try await decoder.decode(#"{"type":"pong"}"#, limit: 100)
        XCTAssertEqual(first.type, "ready"); XCTAssertEqual(second.type, "pong")
        do {
            _ = try await decoder.decode(#"{"type":"pong"}"#, limit: 3)
            XCTFail("Oversized envelopes must be rejected before parsing")
        } catch { XCTAssertEqual(error as? RemoteError, .invalidMessage) }
        do {
            _ = try await decoder.decode("not JSON", limit: 100)
            XCTFail("Malformed wire input must fail")
        } catch {}
    }
}
