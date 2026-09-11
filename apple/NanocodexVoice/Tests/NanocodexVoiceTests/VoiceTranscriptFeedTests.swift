import XCTest
import InboxCore
@testable import NanocodexVoice

final class VoiceTranscriptFeedTests: XCTestCase {
    private func durable(_ text: String, cursor: String) throws -> [TranscriptRow] {
        try transcript([AgentEvent(.object([
            "cursor": .string(cursor), "type": .string("turn_accepted"), "turn_id": .string("voice-" + cursor),
            "input": .string("<realtime_delegation><transcript_delta>user: " + text + "</transcript_delta></realtime_delegation>")
        ]))])
    }
    private func spoken(_ text: String) -> JSON {
        .object(["type": .string("turn.done"), "turn": .object(["role": .string("user"), "transcript": .string(text)])])
    }
    @MainActor func testRecoveredFinalSettlesAgainstDurableAgentHistoryAndAccountResetClearsIt() {
        let voice = VoiceSession(), agent = "conversation"
        voice.transcriptFeed.begin(conversationID: agent, durableRows: [], after: Cursor(rawValue: "10")!)
        voice.startTranscriptPreview(agentID: agent)
        var row = TranscriptRow(id: "coding-final", role: "Agent", text: "Build passed")
        row.cursor = Cursor(rawValue: "11")
        voice.transcriptFeed.reconcile(conversationID: agent, durableRows: [row])
        var recovered = ManagedVoiceEffects(); recovered.undeliveredAnswers = ["Build passed"]
        voice.applyEffectsForTesting(recovered)
        XCTAssertEqual(voice.transcriptFeed.conversations[agent]?.count, 0)
        voice.stop(); voice.clearHistory()
        XCTAssertTrue(voice.transcripts.isEmpty)
        XCTAssertTrue(voice.transcriptFeed.conversations.isEmpty)
    }

    @MainActor func testHistoricalAndPaginatedSpeechCannotAcknowledgeNewCall() throws {
        let voice = VoiceSession(), agent = "conversation"
        let old = try durable("Again", cursor: "10")
        voice.transcriptFeed.reconcile(conversationID: agent, durableRows: old)
        voice.transcriptFeed.begin(conversationID: agent, durableRows: old, after: Cursor(rawValue: "20")!)
        voice.startTranscriptPreview(agentID: agent)
        voice.receiveTranscriptPreview(spoken("Again"))
        XCTAssertEqual(voice.transcriptFeed.conversations[agent]?.map(\.text), ["Again"])
        voice.transcriptFeed.reconcile(conversationID: agent, durableRows: try durable("Again", cursor: "5") + old)
        XCTAssertEqual(voice.transcriptFeed.conversations[agent]?.map(\.text), ["Again"])
        let current = try durable("Again", cursor: "21")
        XCTAssertEqual(current.first?.cursor?.rawValue, "21")
        voice.transcriptFeed.reconcile(conversationID: agent, durableRows: current)
        XCTAssertEqual(voice.transcriptFeed.conversations[agent]?.count, 0)
        voice.stop()
    }
    @MainActor func testCurrentDurableSpeechMayArriveBeforeRealtimeAndStopRetainsPendingText() throws {
        let voice = VoiceSession(), agent = "conversation"
        voice.transcriptFeed.begin(conversationID: agent, durableRows: [], after: Cursor(rawValue: "20")!)
        voice.startTranscriptPreview(agentID: agent)
        voice.transcriptFeed.reconcile(conversationID: agent, durableRows: try durable("Already saved", cursor: "21"))
        voice.receiveTranscriptPreview(spoken("Already saved"))
        XCTAssertEqual(voice.transcriptFeed.conversations[agent]?.count, 0)
        voice.receiveTranscriptPreview(spoken("Keep this until saved"))
        voice.stop()
        XCTAssertEqual(voice.transcriptFeed.conversations[agent]?.map(\.text), ["Keep this until saved"])
        voice.transcriptFeed.clear()
        XCTAssertTrue(voice.transcriptFeed.conversations.isEmpty)
    }
    @MainActor func testNewCallSettlesPreviousSpeechSavedWhileChatWasOffscreen() throws {
        let voice = VoiceSession(), agent = "conversation"
        voice.transcriptFeed.begin(conversationID: agent, durableRows: [], after: Cursor(rawValue: "10")!)
        voice.startTranscriptPreview(agentID: agent)
        voice.receiveTranscriptPreview(spoken("Again")); voice.stop()
        XCTAssertEqual(voice.transcriptFeed.conversations[agent]?.count, 1)
        voice.transcriptFeed.begin(conversationID: agent, durableRows: try durable("Again", cursor: "15"), after: Cursor(rawValue: "20")!)
        XCTAssertEqual(voice.transcriptFeed.conversations[agent]?.count, 0)
        voice.startTranscriptPreview(agentID: agent)
        voice.receiveTranscriptPreview(spoken("Again"))
        XCTAssertEqual(voice.transcriptFeed.conversations[agent]?.map(\.text), ["Again"])
        voice.stop()
    }
}
