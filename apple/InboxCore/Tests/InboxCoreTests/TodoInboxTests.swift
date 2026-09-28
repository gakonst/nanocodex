import Foundation
import XCTest
import InboxCore

final class TodoInboxTests: XCTestCase {
    private func snapshot(_ payload: String) throws -> TodoSnapshot {
        try TodoSnapshot(JSONDecoder().decode(JSON.self, from: Data(payload.utf8)))
    }

    func testGmailReferencesAndCaptureUpdateThroughHTTP() async throws {
        let captureID = "9d998865-50db-4ee2-89fa-b49c5e4f7806"
        let operationID = UUID()
        let fixture = try HTTPFixture { request in
            XCTAssertEqual(request.headers["authorization"], "Bearer \(fixtureKey)")
            if request.method == "GET" {
                XCTAssertEqual(request.path, "/v1/todo")
                return FixtureReply(body: #"{"items":[{"id":"\#(captureID)","body":"Follow up","status":"captured","version":1}],"decisions":[{"id":"new","title":"Same subject","status":"needs_you","version":1,"choices":[],"source_connection_id":"connection-1","source_thread_id":"thread-1","source_message_id":"message-1"},{"id":"legacy","title":"Same subject","status":"needs_you","version":1,"choices":[],"source_url":"https://mail.google.com/"}]}"#)
            }
            XCTAssertEqual(request.method, "PATCH")
            XCTAssertEqual(request.path, "/v1/todo/items/\(captureID)")
            XCTAssertEqual(request.json["version"] as? Int, 1)
            XCTAssertEqual(request.json["status"] as? String, "done")
            XCTAssertEqual(request.json["operation_id"] as? String, operationID.uuidString.lowercased())
            return FixtureReply(body: #"{"item":{"id":"\#(captureID)","body":"Follow up","status":"done","version":2}}"#)
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        let feed = try await client.todoSnapshot()
        XCTAssertEqual(feed.decisions[0].sourceConnectionID, "connection-1")
        XCTAssertEqual(feed.decisions[0].sourceThreadID, "thread-1")
        XCTAssertEqual(feed.decisions[0].sourceMessageID, "message-1")
        XCTAssertNil(feed.decisions[1].sourceConnectionID)
        XCTAssertNil(feed.decisions[1].sourceThreadID)
        XCTAssertNil(feed.decisions[1].sourceMessageID)
        XCTAssertEqual(feed.decisions[1].sourceURL?.absoluteString, "https://mail.google.com/")
        let updated = try await client.updateTodoCapture(feed.captures[0], status: "done", operationID: operationID)
        XCTAssertEqual(updated.status, "done")
        XCTAssertEqual(updated.version, 2)
    }

    // Boundary failures: silently losing ignored results; treating outages as ignores;
    // duplicating positive traces; rejecting older servers; opening unsafe source URLs.
    func testMixedOutcomesPreserveHistoryAndSeparateDeliberateIgnores() throws {
        let result = try snapshot(#"""
        {"items":[{"id":"capture","body":"Follow up","status":"captured","version":1}],
         "decisions":[{"id":"open","title":"Reply?","status":"needs_you","version":1,"choices":[]},
                      {"id":"done","title":"Answered","status":"answered","version":1,"choices":[]}],
         "traces":[{"id":1,"outcome":"reply","decision_id":"open"},
                   {"id":2,"outcome":"no_reply","reason":"no_reply","sender":"Maya <maya@example.test>","subject":"Tuesday","source_url":"https://mail.google.com/mail/u/0/#inbox/123"},
                   {"id":3,"outcome":"filtered"},
                   {"id":4,"outcome":"unavailable","reason":"low_confidence","classifier_outcome":"success"},
                   {"id":5,"outcome":"unavailable","reason":"timeout","classifier_outcome":"timeout"},
                   {"id":6,"outcome":"no_reply","decision_id":"done"}],
         "feed_bounds":{"traces":"recent","trace_limit":100}}
        """#)
        XCTAssertEqual(result.feed(.all).decisions.map(\.id), ["open", "done"])
        XCTAssertEqual(result.feed(.all).traces.map(\.id), [2, 3, 4, 5])
        XCTAssertEqual(result.feed(.all).captures.map(\.id), ["capture"])
        XCTAssertEqual(result.feed(.actionable).decisions.map(\.id), ["open"])
        XCTAssertTrue(result.feed(.actionable).traces.isEmpty)
        XCTAssertTrue(result.feed(.actionable).captures.isEmpty)
        XCTAssertEqual(result.feed(.ignore).traces.map(\.id), [2, 3, 4])
        XCTAssertTrue(result.feed(.ignore).decisions.isEmpty)
        XCTAssertTrue(result.feed(.ignore).captures.isEmpty)
        XCTAssertEqual(result.traces[1].subject, "Tuesday")
        XCTAssertEqual(result.traces[1].reason, "no_reply")
        XCTAssertNotNil(result.traces[1].sourceURL)
    }

    func testOlderSnapshotsAndSparseTracesHaveHonestFallbackAndSafeLinks() throws {
        XCTAssertTrue(try snapshot(#"{"items":[],"decisions":[]}"#).traces.isEmpty)
        let result = try snapshot(#"{"items":[],"decisions":[],"traces":[{"id":1,"outcome":"no_reply","source_url":"javascript:alert(1)"},{"id":2,"outcome":"unavailable","source_url":"https:///"}]}"#)
        XCTAssertEqual(result.traces[0].title, "Email classification")
        XCTAssertNil(result.traces[0].sourceURL)
        XCTAssertNil(result.traces[1].sourceURL)
        XCTAssertFalse(result.traces[1].isIgnored)
    }
}
