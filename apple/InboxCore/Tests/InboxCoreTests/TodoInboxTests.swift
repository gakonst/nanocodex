import Foundation
import XCTest
import InboxCore

final class TodoInboxTests: XCTestCase {
    private func snapshot(_ payload: String) throws -> TodoSnapshot {
        try TodoSnapshot(JSONDecoder().decode(JSON.self, from: Data(payload.utf8)))
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
