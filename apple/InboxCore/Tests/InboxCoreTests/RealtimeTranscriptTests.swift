import XCTest
@testable import InboxCore

final class RealtimeTranscriptTests: XCTestCase {
    func testSpokenHistoryPreservesSpeakersMultilineAndEscaping() throws {
        let input = """
          <realtime_delegation>
          <source>transcript_tail_flush</source>
          <input>Synthetic instruction that must stay hidden</input>
          <transcript_delta>user: fix &lt;x&gt; &amp; ship
        keep &amp;lt; literal
        assistant: I’m on it.</transcript_delta>
        </realtime_delegation>
        """
        let event = try AgentEvent(.object(["cursor": .string("1"), "type": .string("turn_accepted"), "turn_id": .string("t"), "input": .string(input)]))
        let rows = transcript([event, event])
        XCTAssertEqual(rows.map(\.role), ["You", "Agent"])
        XCTAssertEqual(rows.map(\.text), ["fix <x> & ship\nkeep &lt; literal", "I’m on it."])
        XCTAssertEqual(Set(rows.map(\.id)).count, 2)
        var card = AgentCard(id: "agent", title: "Voice")
        card.apply(events: [event])
        XCTAssertEqual(card.preview, "I’m on it.")
    }

    func testInputFallbackAndInternalOnlyEnvelopes() throws {
        XCTAssertEqual(RealtimeTranscript.project("\n<realtime_delegation><input>Ship &quot;it&quot;</input></realtime_delegation>")?.map(\.text), ["Ship \"it\""])
        for input in [
            "<realtime_delegation><source>transcript_tail_flush</source><input>Do not display</input></realtime_delegation>",
            "<realtime_delegation><soruce>transcript_tail_flush</soruce><input>Do not display</input></realtime_delegation>",
            "<realtime_delegation><transcript_delta>unfinished",
            "<realtime_delegation>",
            "<realtime_conversation>internal mode instructions</realtime_conversation>",
        ] {
            XCTAssertEqual(RealtimeTranscript.project(input), [], input)
        }
        for text in ["Use <source> here", "Explain <realtime_delegation> to me", "```xml\n<realtime_delegation>\n```", "2 < 3 & 4 > 1"] {
            XCTAssertNil(RealtimeTranscript.project(text))
        }
        XCTAssertEqual(RealtimeTranscript.project("<realtime_delegation><transcript_delta>…retained tail\nuser: Continue</transcript_delta></realtime_delegation>")?.map(\.speaker), ["assistant", "user"])
    }

    func testStreamingNeverFlashesAnInternalOpeningTag() {
        for opening in ["<realtime_delegation>", "<realtime_conversation>", "<source>", "<soruce>", "<startup_context>"] {
            for length in 1...opening.count {
                XCTAssertEqual(RealtimeTranscript.project(String(opening.prefix(length)), isPartial: true), [], opening)
            }
        }
        XCTAssertNil(RealtimeTranscript.project("<strong> is an HTML tag", isPartial: true))
        XCTAssertNil(RealtimeTranscript.project("<r", isPartial: false))
    }

    func testBootstrapInputWithoutTranscriptRemainsVisibleInNormalChat() throws {
        let input = "<realtime_delegation><source>voice_bootstrap</source><input>Find &lt;this&gt;</input></realtime_delegation>"
        XCTAssertEqual(RealtimeTranscript.project(input)?.map(\.text), ["Find <this>"])
        let event = try AgentEvent(.object(["cursor": .string("1"), "type": .string("turn_accepted"), "turn_id": .string("t"), "input": .string(input)]))
        XCTAssertEqual(transcript([event]).map(\.role), ["You"])
        XCTAssertEqual(transcript([event]).map(\.text), ["Find <this>"])
    }

    func testStreamingHandoffNeverFlashesInputBeforeSpokenTranscript() {
        let prefix = "<realtime_delegation><input>Summarized handoff instruction</input>"
        let transcript = "<transcript_delta>user: Actual speech</transcript_delta>"
        for suffix in ["", "<transcript_delta>", "<transcript_delta>user: Actual"] {
            XCTAssertEqual(RealtimeTranscript.project(prefix + suffix, isPartial: true), [])
        }
        XCTAssertEqual(RealtimeTranscript.project(prefix + transcript, isPartial: true)?.map(\.text), ["Actual speech"])
        XCTAssertEqual(RealtimeTranscript.project(prefix + "</realtime_delegation>", isPartial: true)?.map(\.text), ["Summarized handoff instruction"])
        XCTAssertEqual(RealtimeTranscript.project(prefix + "<transcript_delta>unfinished"), [])
    }

    func testUnknownLifecycleSourceStillCannotExposeSyntheticInput() {
        for source in ["transcript_tail_flush", "future_internal_source"] {
            let input = "<realtime_delegation><source>\(source)</source><input>Synthetic instruction</input></realtime_delegation>"
            XCTAssertEqual(RealtimeTranscript.project(input), [])
        }
        XCTAssertEqual(RealtimeTranscript.project("<realtime_delegation><input>Synthetic instruction</input><source>unfinished"), [])
    }

    func testCRLFTranscriptDoesNotLeaveCarriageReturnsInSpokenRows() {
        let input = "<realtime_delegation><transcript_delta>user: First\r\nsecond\r\nassistant: Reply</transcript_delta></realtime_delegation>"
        XCTAssertEqual(RealtimeTranscript.project(input)?.map(\.text), ["First\nsecond", "Reply"])
        XCTAssertEqual(RealtimeTranscript.project(input)?.map(\.speaker), ["user", "assistant"])
    }
}
