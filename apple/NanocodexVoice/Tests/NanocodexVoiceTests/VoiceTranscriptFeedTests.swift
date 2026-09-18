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
    @MainActor func testPartialMatchDoesNotHideLaterSpeech() throws {
        let voice = VoiceSession(), agent = "conversation"
        voice.transcriptFeed.begin(conversationID: agent, durableRows: [], after: Cursor(rawValue: "10")!)
        voice.startTranscriptPreview(agentID: agent)
        voice.transcriptFeed.reconcile(conversationID: agent, durableRows: try durable("Again", cursor: "11"))
        var effects = ManagedVoiceEffects()
        effects.transcripts = [ManagedVoiceTranscript(speaker: "user", text: "Again", isFinal: false)]
        voice.applyEffectsForTesting(effects)
        XCTAssertEqual(voice.transcriptFeed.conversations[agent]?.map(\.text), ["Again"])
        effects.transcripts = [ManagedVoiceTranscript(speaker: "user", text: "Again tomorrow", isFinal: true)]
        voice.applyEffectsForTesting(effects)
        XCTAssertEqual(voice.transcriptFeed.conversations[agent]?.map(\.text), ["Again tomorrow"])
        voice.stop()
    }

    @MainActor func testStreamedDurableCandidateRefreshesWithoutAcknowledgingTwice() {
        let voice = VoiceSession(), agent = "conversation"
        voice.transcriptFeed.begin(conversationID: agent, durableRows: [], after: Cursor(rawValue: "10")!)
        voice.startTranscriptPreview(agentID: agent)
        var row = TranscriptRow(id: "coding-final", role: "Agent", text: "Build", running: true)
        row.cursor = Cursor(rawValue: "11")
        voice.transcriptFeed.reconcile(conversationID: agent, durableRows: [row])
        var effects = ManagedVoiceEffects(); effects.undeliveredAnswers = ["Build passed"]
        voice.applyEffectsForTesting(effects)
        XCTAssertEqual(voice.transcriptFeed.conversations[agent]?.map(\.text), ["Build passed"])
        row.text = "Build passed"
        voice.transcriptFeed.reconcile(conversationID: agent, durableRows: [row])
        XCTAssertEqual(voice.transcriptFeed.conversations[agent]?.map(\.text), ["Build passed"])
        row.running = false
        voice.transcriptFeed.reconcile(conversationID: agent, durableRows: [row])
        XCTAssertEqual(voice.transcriptFeed.conversations[agent]?.count, 0)
        voice.transcriptFeed.reconcile(conversationID: agent, durableRows: [row])
        voice.applyEffectsForTesting(effects)
        XCTAssertEqual(voice.transcriptFeed.conversations[agent]?.map(\.text), ["Build passed"])
        voice.stop()
    }

    @MainActor func testUnacknowledgedSpeechSurvivesSessionDisplayLimit() throws {
        let voice = VoiceSession(), agent = "conversation"
        voice.transcriptFeed.begin(conversationID: agent, durableRows: [], after: .zero)
        voice.startTranscriptPreview(agentID: agent)
        for index in 1...81 {
            var effects = ManagedVoiceEffects()
            effects.transcripts = [ManagedVoiceTranscript(speaker: "user", text: "Request \(index)", isFinal: true)]
            voice.applyEffectsForTesting(effects)
        }
        XCTAssertEqual(voice.transcripts.count, 80)
        XCTAssertEqual(voice.transcriptFeed.conversations[agent]?.count, 81)
        XCTAssertEqual(voice.transcriptFeed.conversations[agent]?.first?.text, "Request 1")
        voice.stop()
        XCTAssertEqual(voice.transcriptFeed.conversations[agent]?.count, 81)
        voice.transcriptFeed.reconcile(conversationID: agent, durableRows: try durable("Request 1", cursor: "1"))
        XCTAssertEqual(voice.transcriptFeed.conversations[agent]?.count, 80)
        XCTAssertEqual(voice.transcriptFeed.conversations[agent]?.first?.text, "Request 2")
    }

    @MainActor func testDurableCandidatesSurviveMoreThanEightyRowsBeforeLiveSpeech() throws {
        let voice = VoiceSession(), agent = "conversation"
        voice.transcriptFeed.begin(conversationID: agent, durableRows: [], after: .zero)
        voice.startTranscriptPreview(agentID: agent)
        let rows = try (1...81).flatMap { try durable("Request \($0)", cursor: String($0)) }
        voice.transcriptFeed.reconcile(conversationID: agent, durableRows: rows)
        voice.receiveTranscriptPreview(spoken("Request 1"))
        XCTAssertEqual(voice.transcriptFeed.conversations[agent]?.count, 0)
        voice.stop()
    }

    @MainActor func testInternalEnvelopeAndBlankUpdatesPublishNoAssistantPlaceholder() {
        let voice = VoiceSession(), agent = "conversation"
        voice.transcriptFeed.begin(conversationID: agent, durableRows: [], after: .zero)
        voice.startTranscriptPreview(agentID: agent)
        voice.receiveTranscriptPreview(.object([
            "type": .string("turn.done"),
            "turn": .object([
                "role": .string("assistant"),
                "transcript": .string("<realtime_delegation><source>internal-only</source><input>Internal handoff.</input></realtime_delegation>")
            ])
        ]))
        var effects = ManagedVoiceEffects()
        effects.transcripts = [
            ManagedVoiceTranscript(speaker: "assistant", text: "", isFinal: false),
            ManagedVoiceTranscript(speaker: "assistant", text: " \n\t", isFinal: true)
        ]
        voice.applyEffectsForTesting(effects)
        voice.receiveTranscriptPreview(.object([
            "type": .string("input_transcript.added"), "item": .object(["text": .string("Can you hear")])
        ]))
        XCTAssertEqual(voice.transcripts.map(\.speaker), ["user"])
        XCTAssertEqual(voice.transcriptFeed.conversations[agent]?.map(\.text), ["Can you hear"])
        voice.receiveTranscriptPreview(.object([
            "type": .string("output_transcript.added"), "item": .object(["text": .string("I can")])
        ]))
        XCTAssertEqual(voice.transcriptFeed.conversations[agent]?.map(\.text), ["Can you hear", "I can"])
        voice.stop()
    }

}
