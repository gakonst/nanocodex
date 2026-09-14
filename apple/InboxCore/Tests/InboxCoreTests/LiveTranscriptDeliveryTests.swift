import XCTest
@testable import InboxCore

#if !os(Linux)
final class LiveTranscriptDeliveryTests: XCTestCase {
    func testCommentaryAndAnswerReachProjectionBeforeStreamCloses() async throws {
        let delivered = expectation(description: "Commentary and answer are projected before EOF")
        delivered.expectedFulfillmentCount = 2
        let commentary = #"{"type":"event","turn_id":"owned-turn","event":{"type":"assistant.delta","payload":{"text":"Checking now","phase":"commentary","item_id":"commentary"}}}"#
        let body = #"{"type":"event","turn_id":"owned-turn","event":{"type":"assistant.delta","payload":{"text":"Live answer","phase":"final_answer","item_id":"answer"}}}"#
        let fixture = try HTTPFixture { _ in
            .init(headers: ["Content-Type": "text/event-stream"],
                  body: "id: 1\ndata: \(commentary)\n\nid: 2\ndata: \(body)\n\n", streaming: true)
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try .init(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        let projector = TranscriptStreamProjection()
        let reader = Task {
            try await client.stream("owned-agent", after: .zero) { frame in
                guard let event = frame.event else { return }
                let rows = try? await projector.rows([event])
                XCTAssertEqual(rows?.first?.text, event.cursor.rawValue == "1" ? "Checking now" : "Live answer")
                XCTAssertEqual(rows?.first?.running, true)
                XCTAssertEqual(rows?.first?.phase, event.cursor.rawValue == "1" ? "commentary" : "final_answer")
                delivered.fulfill()
            }
        }
        await fulfillment(of: [delivered], timeout: 2)
        reader.cancel()
        do { try await reader.value; XCTFail("Cancelled stream returned normally") }
        catch is CancellationError { }
        catch let error as URLError { XCTAssertEqual(error.code, .cancelled) }
    }
}
#endif
