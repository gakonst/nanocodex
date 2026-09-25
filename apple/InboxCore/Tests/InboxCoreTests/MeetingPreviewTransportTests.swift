import Foundation
import XCTest
import InboxCore

final class MeetingPreviewTransportTests: XCTestCase {
    func testFinalSegmentUsesScopedPreviewAndExactRevision() async throws {
        let capture = UUID()
        var calls = 0
        let fixture = try HTTPFixture { request in
            calls += 1
            XCTAssertEqual(request.path, "/v1/meetings/\(capture.uuidString.lowercased())/preview")
            XCTAssertEqual(request.headers["authorization"], "Bearer " + fixtureKey)
            if request.method == "POST" {
                XCTAssertEqual(request.json["revision"] as? Int, 1)
                XCTAssertEqual(request.json["delta"] as? String, "Decision: launch the prototype.")
                return FixtureReply(body: "{\"capture_id\":\"\(capture.uuidString.lowercased())\",\"revision\":1,\"summary\":\"Prototype launch decided.\",\"summary_revision\":1,\"status\":\"updated\"}")
            }
            if request.method == "GET" {
                return FixtureReply(body: "{\"capture_id\":\"\(capture.uuidString.lowercased())\",\"revision\":1,\"summary\":\"Prototype launch decided.\",\"summary_revision\":1,\"status\":\"unchanged\"}")
            }
            XCTAssertEqual(request.method, "DELETE")
            return FixtureReply(status: 204, body: "")
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try .init(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        let updated = try await client.updateMeetingPreview(captureID: capture, revision: 1, delta: "Decision: launch the prototype.")
        XCTAssertEqual(updated.summary, "Prototype launch decided.")
        XCTAssertEqual(updated.summaryRevision, 1)
        let recovered = try await client.meetingPreview(captureID: capture)
        XCTAssertEqual(recovered.revision, 1)
        try await client.closeMeetingPreview(captureID: capture)
        XCTAssertEqual(calls, 3)
    }

    func testRejectsWrongCaptureOrOversizedDeltaBeforeNetwork() async throws {
        let capture = UUID()
        let fixture = try HTTPFixture { _ in FixtureReply(body: "{\"capture_id\":\"\(UUID().uuidString.lowercased())\",\"revision\":1,\"summary\":\"wrong capture\",\"summary_revision\":1,\"status\":\"updated\"}") }
        defer { fixture.close() }
        let client = ManagedClient(credential: try .init(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        do {
            _ = try await client.updateMeetingPreview(captureID: capture, revision: 1, delta: String(repeating: "x", count: 4097))
            XCTFail("Oversized Speech segment should be split before calling transport")
        } catch APIError.invalidResponse { }
        do {
            _ = try await client.updateMeetingPreview(captureID: capture, revision: 1, delta: "hello")
            XCTFail("Cross-capture preview is not a valid receipt")
        } catch APIError.invalidResponse { }
    }
}
