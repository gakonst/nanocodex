import Foundation
import XCTest
@testable import InboxCore

final class AgentActivityTests: XCTestCase {
    private func card(_ id: String, status: String, cursor: String = "10") -> AgentCard {
        var card = AgentCard(id: id, title: id)
        card.checked = true; card.status = status; card.latestCursor = Cursor(rawValue: cursor)!
        if status == "Running" { card.activeTurns = ["turn-" + id] }
        return card
    }

    func testCountsUseAllConversationsAndRowsPrioritizeActionableOutcomes() {
        let cards = [card("running", status: "Running"), card("ready", status: "Ready"),
                     card("failed", status: "Failed"), card("read", status: "Ready"),
                     card("deferred", status: "Failed"), card("idle", status: "Idle"),
                     card("stopped", status: "Stopped"), card("also-running", status: "Running")]
        let state = AgentActivitySnapshot.make(cards: cards, seen: ["read": Cursor(rawValue: "10")!],
                                               deferred: ["deferred": Cursor(rawValue: "10")!], paused: false)
        XCTAssertEqual(state.running, 2)
        XCTAssertEqual(state.ready, 1)
        XCTAssertEqual(state.failed, 1)
        XCTAssertEqual(state.entries.map(\.id), ["failed", "also-running"])
        XCTAssertTrue(state.paused, "Unobserved state must not claim freshness")
    }

    func testFailedReadDoesNotBecomeFailedTurnAndSeenRunningRemainsVisible() {
        var running = card("a", status: "Running")
        running.error = "offline"
        let state = AgentActivitySnapshot.make(cards: [running], seen: ["a": Cursor(rawValue: "10")!],
                                               deferred: ["a": Cursor(rawValue: "10")!], paused: false)
        XCTAssertEqual(state.failed, 0)
        XCTAssertEqual(state.running, 1)
        XCTAssertEqual(state.entries.first?.detail, "Status unavailable · open to reconnect")
        XCTAssertTrue(state.paused)
    }

    func testCurrentActivityExcludesPreviousTurnsAndToolArguments() throws {
        var running = card("a", status: "Running")
        let event = try AgentEvent(.object(["type": .string("turn_accepted"), "cursor": .string("11"), "turn_id": .string("turn-a")]))
        var old = TranscriptRow(id: "old", role: "Tool", text: "secret", tool: .init(name: "read_file", arguments: .object(["path": .string("private")])) )
        old.turnID = "previous-turn"
        running.apply(events: [event], transcriptRows: [old])
        XCTAssertEqual(running.activitySummary, "Working")
        var current = TranscriptRow(id: "current", role: "Tool", text: "secret", tool: .init(name: "apply_patch", arguments: .string("private patch")))
        current.turnID = "turn-a"
        running.apply(events: [event], transcriptRows: [old, current])
        XCTAssertEqual(running.activitySummary, "Edit files")
        XCTAssertNotNil(running.observedAt)
        let snapshot = AgentActivitySnapshot.make(cards: [running], seen: [:], deferred: [:], paused: false)
        XCTAssertFalse(snapshot.paused)
        XCTAssertEqual(snapshot.entries.first?.detail, "Edit files")
        XCTAssertFalse(String(decoding: try JSONEncoder().encode(snapshot), as: UTF8.self).contains("private"))
    }

    func testPayloadIsBoundedEvenWithLargeUnicodeTitlesAndLargeRoster() throws {
        var cards = (0..<256).map { card("agent-\($0)", status: "Running") }
        for index in cards.indices { cards[index].title = String(repeating: "👩🏽‍💻", count: 1000) }
        let snapshot = AgentActivitySnapshot.make(cards: cards, seen: [:], deferred: [:], paused: true)
        XCTAssertEqual(snapshot.running, 256)
        XCTAssertEqual(snapshot.entries.count, 2)
        XCTAssertLessThan(try JSONEncoder().encode(snapshot).count, 2000)
        XCTAssertTrue(snapshot.entries.allSatisfy { $0.title.utf8.count <= 160 })
        XCTAssertEqual(try JSONDecoder().decode(AgentActivitySnapshot.self, from: JSONEncoder().encode(snapshot)), snapshot)
    }

    func testLinksRoundTripAndRejectOtherAccountsAndAmbiguousDestinations() throws {
        let id = "agent/with ? + # Unicode 🐕"
        let url = AgentActivityLink.url(account: "account-a", agentID: id)
        XCTAssertEqual(AgentActivityLink.destination(url, account: "account-a"), id)
        XCTAssertNil(AgentActivityLink.destination(url, account: "account-b"))
        for text in ["nanocodex://activity?account=a&account=b&agent=x", "nanocodex://activity?account=a&agent=x&agent=y",
                     "nanocodex://activity/path?account=a&agent=x", "https://activity?account=a&agent=x",
                     "nanocodex://activity?account=a&agent="] {
            XCTAssertNil(AgentActivityLink.destination(try XCTUnwrap(URL(string: text)), account: "a"))
        }
    }

    func testFailedDeliverySurvivesSeenAndDeferredWithoutDoubleCounting() {
        var message = PendingMessage(agentID: "failed", input: "private input", predecessor: "")
        message.phase = .failed; message.error = "private transport details"
        let cards = [card("failed", status: "Failed"), card("active", status: "Running")]
        var state = AgentActivitySnapshot.make(cards: cards, seen: [:], deferred: [:], paused: false, pending: [message])
        XCTAssertEqual(state.failed, 1)
        XCTAssertEqual(state.deliveryFailures, 1)
        XCTAssertEqual(state.needsAttention, 1)
        XCTAssertEqual(state.total, 2)
        XCTAssertEqual(state.entries.map(\.id), ["failed", "active"])
        XCTAssertEqual(state.entries.first?.status, "delivery")
        XCTAssertEqual(state.entries.first?.action, "Retry delivery")
        XCTAssertFalse(state.entries.first!.detail.contains("private"))
        state = .make(cards: cards, seen: ["failed": Cursor(rawValue: "10")!],
                      deferred: ["failed": Cursor(rawValue: "10")!], paused: false, pending: [message])
        XCTAssertEqual(state.failed, 0)
        XCTAssertEqual(state.needsAttention, 1, "Reviewing a result does not confirm message delivery")
    }

    func testQueueCountsExcludeSubmittingStartingCancelledAndUnknownAgents() {
        let pending = PendingMessage.Phase.allTestCases.enumerated().map { index, phase in
            var message = PendingMessage(agentID: "active", input: "private", predecessor: "", id: "m\(index)")
            message.phase = phase; return message
        }
        var unknown = PendingMessage(agentID: "deleted", input: "", predecessor: "")
        unknown.phase = .queued
        let state = AgentActivitySnapshot.make(cards: [card("active", status: "Running")], seen: [:], deferred: [:],
                                               paused: false, pending: pending + [unknown])
        XCTAssertEqual(state.queued, 1)
        XCTAssertEqual(state.entries.first?.queued, 1)
        XCTAssertEqual(state.total, 1)
    }

    func testFailureReasonAndReplySurviveLaterInternalEvents() throws {
        var failed = card("a", status: "Running")
        func event(_ cursor: String, _ type: String, _ fields: [String: JSON] = [:]) throws -> AgentEvent {
            try AgentEvent(.object(fields.merging(["type": .string(type), "cursor": .string(cursor), "turn_id": .string("turn-a")]) { _, new in new }))
        }
        failed.apply(events: [try event("11", "turn_failed", ["error": .string("Browser disconnected. Reconnect to continue.")])])
        failed.apply(events: [try event("12", "internal")], transcriptRows: [])
        var state = AgentActivitySnapshot.make(cards: [failed], seen: [:], deferred: [:], paused: false)
        XCTAssertEqual(state.entries.first?.detail, "Browser disconnected. Reconnect to continue.")
        failed.apply(events: [try event("13", "turn_accepted")])
        XCTAssertEqual(failed.outcomeSummary, "")
        failed.apply(events: [try event("14", "turn_completed", ["final_message": .string("**Fixed reconnect.** All 18 checks pass.")])])
        state = .make(cards: [failed], seen: [:], deferred: [:], paused: false)
        XCTAssertEqual(state.entries.first?.detail, "Fixed reconnect. All 18 checks pass.")
        XCTAssertEqual(state.headline, "1 ready to review")
    }

    func testOnlyExplicitCurrentTurnCommentaryBecomesProgressExcerpt() throws {
        var running = card("a", status: "Running")
        var commentary = TranscriptRow(id: "current", role: "Agent", text: "Checking reconnect behavior across two devices.")
        commentary.turnID = "turn-a"; commentary.phase = "commentary"
        let event = try AgentEvent(.object(["type": .string("event"), "cursor": .string("11"), "turn_id": .string("turn-a")]))
        running.apply(events: [event], transcriptRows: [commentary])
        XCTAssertEqual(running.activityDetail, commentary.text)
        var thinking = TranscriptRow(id: "thinking", role: "Thinking", text: "private reasoning")
        thinking.turnID = "turn-a"
        running.apply(events: [event], transcriptRows: [commentary, thinking])
        XCTAssertEqual(running.activityDetail, "")
        XCTAssertEqual(running.activitySummary, "Thinking")
        try running.apply(state: .object(["agent_id": .string("a"), "latest_event_cursor": .string("12"), "active_turns": .array([.string("new-turn")])]))
        XCTAssertEqual(running.activitySummary, "Working")
        XCTAssertEqual(running.activityDetail, "")
    }

    func testExcerptsStripCodeBlocksAndLinkTargets() {
        XCTAssertEqual(AgentActivityText.excerpt("# Done\n**Fixed** [reconnect](https://example.com/private).\n```sh\nsecret command\n```\nReady to review."),
                       "Done Fixed reconnect. Ready to review.")
    }

    func testPreviousActivityPayloadStillDecodes() throws {
        let state = AgentActivitySnapshot.make(cards: [card("a", status: "Running")], seen: [:], deferred: [:], paused: true)
        var json = try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(state)) as? [String: Any])
        for key in ["deliveryFailures", "queued", "attentionCount", "conversationCount"] { json.removeValue(forKey: key) }
        var entries = try XCTUnwrap(json["entries"] as? [[String: Any]])
        entries[0].removeValue(forKey: "action"); entries[0].removeValue(forKey: "queued"); json["entries"] = entries
        let decoded = try JSONDecoder().decode(AgentActivitySnapshot.self, from: JSONSerialization.data(withJSONObject: json))
        XCTAssertEqual(decoded.total, 1)
        XCTAssertEqual(decoded.headline, "1 running")
    }
}

private extension PendingMessage.Phase {
    static let allTestCases: [Self] = [.submitting, .queued, .starting, .cancelling]
}
