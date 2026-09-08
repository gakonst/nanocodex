import XCTest
import InboxCore
@testable import NanocodexVoice

final class ManagedVoiceTests: XCTestCase {
    private let agent = "019d2f5d-7491-8000-8000-000000000001"

    func testPrefetchUsesTheBoundedReadEndpointWithoutTurnAdmission() async throws {
        let session = ManagedVoiceProtocol.sessionID()
        var requests: [FixtureRequest] = []
        let fixture = try HTTPFixture { request in
            requests.append(request)
            return .init(body: #"{"prefetched":true}"#)
        }
        defer { fixture.close() }
        let transport = try ManagedVoiceTransport(credential: .init(origin: fixture.origin, apiKey: fixtureKey), agentID: agent, configuration: fixture.configuration)
        try await transport.prefetch(sessionID: session, query: "When is Elena's birthday?")
        XCTAssertEqual(requests.count, 1)
        XCTAssertEqual(requests[0].path, "/v1/agents/\(agent)/realtime/prefetch")
        XCTAssertEqual(requests[0].method, "POST")
        XCTAssertEqual(requests[0].json["voice_session_id"] as? String, session)
        XCTAssertEqual(requests[0].json["query"] as? String, "When is Elena's birthday?")
        do {
            try await transport.prefetch(sessionID: session, query: String(repeating: "x", count: 513))
            XCTFail("Oversized speculative queries must be rejected before network I/O")
        } catch {}
        XCTAssertEqual(requests.count, 1)
        await transport.close()
    }

    func testSpeechPlaysWithoutBootstrapAndOnlyProviderHandoffsAdmitAgentWork() throws {
        let voice = try ManagedVoiceProtocol()
        voice.bindSession("call-1")
        XCTAssertEqual(voice.sidebandOpened().playbackEnabled, true)
        let partial = voice.realtimeMessage(.object(["type": .string("input_transcript.added"), "item": .object([
            "text": .string("When is Elena's birthday?")
        ])]))
        XCTAssertNil(partial.delegation)
        XCTAssertNil(partial.prefetch)
        let first = voice.realtimeMessage(.object(["type": .string("turn.done"), "turn": .object([
            "role": .string("user"), "transcript": .string("When is Elena's birthday?")
        ])]))
        XCTAssertNil(first.delegation)
        let handoff = voice.realtimeMessage(.object(["type": .string("delegation.created"), "item": .object([
            "type": .string("delegation"), "target": .string("client"), "id": .string("lookup"),
            "content": .array([.object(["type": .string("input_text"), "text": .string("Search saved memory for the birthday")])])
        ])]))
        let input = try XCTUnwrap(handoff.delegation).formattedInput
        XCTAssertFalse(input.contains("voice_bootstrap"))
        XCTAssertTrue(input.contains("Search saved memory for the birthday"))
        XCTAssertTrue(input.contains("Elena's birthday?"))
        let result = voice.agentEvent(.object(["type": .string("assistant.message"), "payload": .object([
            "text": .string("The saved date is December 22.")
        ])]))
        XCTAssertEqual(voice.sidebandOpened().playbackEnabled, true)
        XCTAssertFalse(result.frames.isEmpty)
        XCTAssertEqual(voice.sidebandOpened().frames, result.frames)
        voice.framesSent(result.frames.count)
        XCTAssertTrue(voice.sidebandOpened().frames.isEmpty)
    }

    func testBackgroundMemoryUpdatesSurviveReconnectWithoutSpeakingOrCompletingDelegation() throws {
        let voice = try ManagedVoiceProtocol()
        _ = voice.realtimeMessage(.object(["type": .string("delegation.created"), "item": .object([
            "type": .string("delegation"), "target": .string("client"), "id": .string("lookup"),
            "content": .array([.object(["type": .string("input_text"), "text": .string("Remember the correction")])])
        ])]))
        let effects = voice.context("Saved-memory update: new birthday")
        XCTAssertTrue(effects.transcripts.isEmpty)
        XCTAssertEqual(effects.frames.first?["channel"].string, "commentary")
        XCTAssertEqual(effects.frames.first?["type"].string, "session.context.append")
        XCTAssertEqual(voice.sidebandOpened().frames, effects.frames)
        voice.framesSent(1)
        XCTAssertTrue(voice.sidebandOpened().frames.isEmpty)
        let reply = voice.agentEvent(.object(["type": .string("assistant.message"), "payload": .object(["text": .string("Saved it.")])]))
        XCTAssertEqual(reply.frames.first?["type"].string, "delegation.context.append")
        XCTAssertEqual(reply.frames.first?["delegation_item_id"].string, "lookup")
        let longContext = String(repeating: "🦊", count: 4096)
        XCTAssertEqual(voice.context(longContext).frames.map { $0["content"].array[0]["text"].string }.joined(), longContext)
    }

    func testDurableLifecycleRetriesSameIdentityAndValidatesReceipts() async throws {
        let session = ManagedVoiceProtocol.sessionID()
        var calls: [FixtureRequest] = [], starts = 0
        let fixture = try HTTPFixture { request in
            calls.append(request)
            XCTAssertEqual(request.method, "POST")
            XCTAssertEqual(request.headers["authorization"], "Bearer \(fixtureKey)")
            XCTAssertNil(request.headers["cookie"])
            var reply: [String: Any] = ["voice_session_id": request.json["voice_session_id"] ?? "", "operation_id": request.json["operation_id"] ?? "", "context": ["workspace": "/workspace", "history": []]]
            if request.path.hasSuffix("/start") {
                starts += 1
                if starts == 1 { return .init(status: 503) }
                XCTAssertEqual(Set(request.json.keys), ["voice_session_id", "operation_id"])
            } else if request.path.hasSuffix("/delegate") {
                XCTAssertEqual(request.json["input"] as? String, "<realtime_delegation>hello</realtime_delegation>")
                reply["route"] = "started"; reply["turn_id"] = "voice-turn"
            } else if request.path.hasSuffix("/stop") { reply["stopped"] = true; reply["context"] = [] }
            return .init(body: String(data: try! JSONSerialization.data(withJSONObject: reply), encoding: .utf8)!)
        }
        defer { fixture.close() }
        let transport = try ManagedVoiceTransport(credential: .init(origin: fixture.origin, apiKey: fixtureKey), agentID: agent, configuration: fixture.configuration)
        let context = try await transport.start(sessionID: session, operationID: "start-once")
        XCTAssertEqual(context["workspace"].string, "/workspace")
        XCTAssertEqual(calls.prefix(2).map(\.body), [calls[0].body, calls[0].body])
        let routed = try await transport.delegate(sessionID: session, operationID: "delegation-once", input: "<realtime_delegation>hello</realtime_delegation>")
        XCTAssertEqual(routed.turnID, "voice-turn"); XCTAssertEqual(routed.route, "started")
        try await transport.cancel(turnID: routed.turnID)
        _ = try await transport.stop(sessionID: session, operationID: "stop-once")
        XCTAssertEqual(calls.last?.path, "/v1/agents/\(agent)/realtime/stop")
        await transport.close()
        do { _ = try await transport.start(sessionID: session, operationID: "after-close"); XCTFail("Closed voice reopened") }
        catch let error as ManagedError { XCTAssertEqual(error.code, "cancelled") }
    }

    func testSDPUsesStrictManagedEnvelopeAndSanitizedLocation() async throws {
        let session = ManagedVoiceProtocol.sessionID()
        let fixture = try HTTPFixture { request in
            XCTAssertEqual(request.path, "/v1/agents/019d2f5d-7491-8000-8000-000000000001/realtime/calls")
            XCTAssertEqual(request.headers["x-nanocodex-voice-session-id"], session)
            XCTAssertEqual(request.headers["authorization"], "Bearer \(fixtureKey)")
            XCTAssertNil(request.headers["openai-alpha"]); XCTAssertNil(request.headers["chatgpt-account-id"])
            XCTAssertEqual(Set(request.json.keys), ["sdp", "session"])
            let body = request.json["session"] as! [String: Any]
            XCTAssertEqual(Set(body.keys), ["model", "instructions", "audio", "delegation"])
            XCTAssertEqual(body["model"] as? String, "gpt-live-1-codex")
            XCTAssertEqual((body["delegation"] as? [String: String])?["type"], "client")
            XCTAssertFalse(String(data: request.body, encoding: .utf8)!.contains(fixtureKey))
            return .init(status: 201, headers: ["Content-Type": "application/sdp", "x-nanocodex-realtime-location": "https://provider.invalid/v1/realtime/calls/rtc_owned?ignored=true"], body: "v=0\r\nanswer")
        }
        defer { fixture.close() }
        let transport = try ManagedVoiceTransport(credential: .init(origin: fixture.origin, apiKey: fixtureKey), agentID: agent, configuration: fixture.configuration)
        let call = try await transport.call(sdp: "v=0\r\noffer", instructions: "Use the coding agent.", sessionID: session)
        XCTAssertEqual(call.sdp, "v=0\r\nanswer"); XCTAssertEqual(call.callID, "rtc_owned")
        await transport.close()
    }

    func testSubscriptionSettingsUseSharedRustSessionAndReconnectQueue() throws {
        let settings = VoiceSettings(voice: "maple", instructions: "Speak Greek.", pace: .slow,
                                     updates: .results, acknowledgements: false)
        let session = try ManagedVoiceProtocol.session(instructions: "Use the coding agent.", settings: settings)
        XCTAssertEqual(session["model"].string, "gpt-live-1-codex")
        XCTAssertEqual(session["audio"]["output"]["voice"].string, "maple")
        XCTAssertEqual(session["delegation"]["ack_filler"], .bool(false))
        XCTAssertTrue(session["instructions"].string.hasPrefix("Use the coding agent."))
        XCTAssertTrue(session["instructions"].string.hasSuffix("Speak Greek."))
        let voice = try ManagedVoiceProtocol(settings: settings)
        let speech = try voice.appendSpeech("Read this aloud.")
        XCTAssertEqual(speech.frames.first?["channel"].string, "speakable")
        let context = try voice.appendText("The user selected a different file.", role: "developer")
        XCTAssertEqual(context.frames.first?["type"].string, "session.context.append")
        XCTAssertEqual(context.frames.first?["content"].array.first?["text"].string, "The user selected a different file.")
        XCTAssertEqual(voice.sidebandOpened().frames, speech.frames + context.frames)
        voice.framesSent(2)
        XCTAssertTrue(voice.sidebandOpened().frames.isEmpty)
        XCTAssertThrowsError(try voice.appendText("Invalid role", role: "system"))
        let longSpeech = String(repeating: "🦊", count: 4096)
        XCTAssertEqual(try voice.appendSpeech(longSpeech).frames.map { $0["content"].array[0]["text"].string }.joined(), longSpeech)
        XCTAssertNoThrow(try ManagedVoiceProtocol(settings: VoiceSettings(instructions: longSpeech)))
        XCTAssertThrowsError(try ManagedVoiceProtocol(settings: VoiceSettings(voice: "voice_custom")))
    }

    func testLongStartupHistoryUsesBoundedFramesAndRetainsItsSelectedContext() throws {
        let history: [JSON] = (0..<6).map { index in .object([
            "role": .string(index.isMultiple(of: 2) ? "user" : "assistant"),
            "content": .array([.object([
                "text": .string("FIRST \(index) " + String(repeating: "Ελληνικά 🦊 ", count: 40) + " LAST \(index)")
            ])])
        ]) }
        let context: JSON = .object(["history": .array(history)])
        let frames = ManagedVoiceProtocol.startupContextFrames(context)
        XCTAssertGreaterThan(frames.count, 1)
        let chunks = frames.map { $0["content"].array[0]["text"].string }
        XCTAssertGreaterThan(chunks.joined().utf8.count, 2_000)
        XCTAssertTrue(chunks.allSatisfy { $0.utf8.count <= 500 })
        XCTAssertTrue(frames.allSatisfy { $0["type"].string == "session.context.append" && $0["channel"].string == "commentary" })
        XCTAssertTrue(ManagedVoiceProtocol.instructions(context: context).hasSuffix(chunks.joined()))
        XCTAssertTrue(chunks.joined().contains("FIRST"))
        XCTAssertTrue(chunks.joined().contains("LAST"))
    }

    func testStartRecoversEgressTimeoutWithFreshOperationAndSameVoiceSession() async throws {
        let session = ManagedVoiceProtocol.sessionID()
        var calls: [FixtureRequest] = []
        let fixture = try HTTPFixture { request in
            calls.append(request)
            XCTAssertEqual(request.json["voice_session_id"] as? String, session)
            if calls.count == 1 {
                return .init(status: 500, body: #"{"error":"realtime_start_failed","message":"Cloudflare Agent EGRESS startup validation timed out"}"#)
            }
            // Failed startup operations are permanently blocked by the service.
            if request.json["operation_id"] as? String == "blocked-start" {
                return .init(status: 409, body: #"{"error":"operation_blocked"}"#)
            }
            if calls.count == 2 { return .init(status: 503) }
            return .init(body: String(data: try! JSONSerialization.data(withJSONObject: [
                "voice_session_id": session, "operation_id": request.json["operation_id"]!,
                "context": ["workspace": "/workspace", "history": []]
            ]), encoding: .utf8)!)
        }
        defer { fixture.close() }
        let transport = try ManagedVoiceTransport(credential: .init(origin: fixture.origin, apiKey: fixtureKey), agentID: agent, configuration: fixture.configuration)
        let context = try await transport.start(sessionID: session, operationID: "blocked-start")
        XCTAssertEqual(context["workspace"].string, "/workspace")
        XCTAssertEqual(calls.count, 3)
        XCTAssertNotEqual(calls[0].json["operation_id"] as? String, calls[1].json["operation_id"] as? String)
        XCTAssertEqual(calls[1].body, calls[2].body, "Ordinary transport retries must retain their operation identity")
        await transport.close()
    }

    func testStartupRecoveryStopsAfterThreeAttempts() async throws {
        var operations: [String] = []
        let fixture = try HTTPFixture { request in
            operations.append(request.json["operation_id"] as! String)
            return .init(status: 500, body: #"{"error":"realtime_start_failed","message":"Cloudflare Agent EGRESS startup validation timed out"}"#)
        }
        defer { fixture.close() }
        let transport = try ManagedVoiceTransport(credential: .init(origin: fixture.origin, apiKey: fixtureKey), agentID: agent, configuration: fixture.configuration)
        do {
            _ = try await transport.start(sessionID: ManagedVoiceProtocol.sessionID(), operationID: "start")
            XCTFail("Repeated startup failures did not settle")
        } catch let error as ManagedError {
            XCTAssertEqual(error.code, "voice_startup_timeout")
            XCTAssertFalse(error.message.contains("EGRESS"))
        }
        XCTAssertEqual(operations.count, 3)
        XCTAssertEqual(Set(operations).count, 3)
        await transport.close()
    }

    func testStartupRecoveryDoesNotReplayAmbiguousOrUnrelatedFailures() async throws {
        for (status, code, message) in [
            (409, "operation_blocked", "realtime operation outcome is ambiguous after interruption"),
            (500, "realtime_start_failed", "an unrelated failure"),
            (500, "unrelated_failure", "Cloudflare Agent EGRESS startup validation timed out"),
            (403, "forbidden", "Cloudflare Agent EGRESS startup validation timed out")
        ] {
            var calls = 0
            let fixture = try HTTPFixture { _ in
                calls += 1
                return .init(status: status, body: String(data: try! JSONSerialization.data(withJSONObject: ["error": code, "message": message]), encoding: .utf8)!)
            }
            let transport = try ManagedVoiceTransport(credential: .init(origin: fixture.origin, apiKey: fixtureKey), agentID: agent, configuration: fixture.configuration)
            do {
                _ = try await transport.start(sessionID: ManagedVoiceProtocol.sessionID(), operationID: "start")
                XCTFail("Failed startup was accepted")
            } catch let error as ManagedError { XCTAssertEqual(error.code, code) }
            XCTAssertEqual(calls, 1)
            await transport.close(); fixture.close()
        }
    }

    func testClosingVoiceCancelsStartupRecovery() async throws {
        let received = expectation(description: "Startup timeout returned")
        var calls = 0
        let fixture = try HTTPFixture { _ in
            calls += 1; received.fulfill()
            return .init(status: 500, body: #"{"error":"realtime_start_failed","message":"Cloudflare Agent EGRESS startup validation timed out"}"#)
        }
        defer { fixture.close() }
        let transport = try ManagedVoiceTransport(credential: .init(origin: fixture.origin, apiKey: fixtureKey), agentID: agent, configuration: fixture.configuration)
        let task = Task { try await transport.start(sessionID: ManagedVoiceProtocol.sessionID(), operationID: "start") }
        await fulfillment(of: [received], timeout: 3)
        await transport.close()
        do { _ = try await task.value; XCTFail("Closed voice retried startup") }
        catch let error as ManagedError { XCTAssertEqual(error.code, "cancelled") }
        XCTAssertEqual(calls, 1)
    }

    func testReceiptMismatchAndInvalidSessionFailClosed() async throws {
        var requests = 0
        let fixture = try HTTPFixture { _ in requests += 1; return .init(body: #"{"voice_session_id":"wrong","operation_id":"wrong","context":[]}"#) }
        defer { fixture.close() }
        let transport = try ManagedVoiceTransport(credential: .init(origin: fixture.origin, apiKey: fixtureKey), agentID: agent, configuration: fixture.configuration)
        for session in ["not-a-session", ManagedVoiceProtocol.sessionID()] {
            do { _ = try await transport.start(sessionID: session, operationID: "operation"); XCTFail("Invalid lifecycle accepted") }
            catch let error as ManagedError { XCTAssertEqual(error.code, "invalid_response") }
        }
        XCTAssertEqual(requests, 1)
        await transport.close()
    }


    func testCloseFencesAnInflightLifecycleResponse() async throws {
        let received = expectation(description: "Start admitted")
        let fixture = try HTTPFixture { request in
            received.fulfill()
            return .init(body: String(data: try! JSONSerialization.data(withJSONObject: ["voice_session_id": request.json["voice_session_id"]!, "operation_id": request.json["operation_id"]!, "context": []]), encoding: .utf8)!, delay: 0.3)
        }
        defer { fixture.close() }
        let transport = try ManagedVoiceTransport(credential: .init(origin: fixture.origin, apiKey: fixtureKey), agentID: agent, configuration: fixture.configuration)
        let task = Task { try await transport.start(sessionID: ManagedVoiceProtocol.sessionID(), operationID: "pending") }
        await fulfillment(of: [received], timeout: 3); await transport.close()
        do { _ = try await task.value; XCTFail("Old voice start escaped close") }
        catch let error as ManagedError { XCTAssertEqual(error.code, "cancelled") }
    }


    func testEventStreamResumesExactCursorDeduplicatesReplayAndReportsTerminalFailure() async throws {
        var queries: [String] = []
        let fixture = try HTTPFixture { request in
            XCTAssertEqual(request.headers["authorization"], "Bearer \(fixtureKey)")
            XCTAssertNil(request.headers["cookie"])
            if request.path == "/v1/agents/\(self.agent)" {
                return .init(body: #"{"latest_event_cursor":"9007199254740992"}"#)
            }
            queries.append(request.query ?? "")
            let first = "id: 9007199254740993\ndata: {\"type\":\"event\",\"turn_id\":\"voice-turn\",\"event\":{\"type\":\"assistant.delta\",\"payload\":{\"text\":\"Hello\"}}}\n\n"
            let tail = queries.count == 1 ? ": cursor 9007199254740994\n\n" : "id: 9007199254740995\ndata: {\"type\":\"stream_failed\"}\n\n"
            return .init(headers: ["Content-Type": "text/event-stream"], body: first + tail)
        }
        defer { fixture.close() }
        let transport = try ManagedVoiceTransport(credential: .init(origin: fixture.origin, apiKey: fixtureKey), agentID: agent, configuration: fixture.configuration)
        let stream = try await transport.events()
        let received = try await eventDeadline {
            var received: [String] = []
            do {
                for try await event in stream { received.append(event.cursor.rawValue) }
                XCTFail("Terminal stream failure silently ended")
            } catch let error as ManagedError { XCTAssertEqual(error.code, "stream_failed") }
            return received
        }
        XCTAssertEqual(received, ["9007199254740993"])
        XCTAssertEqual(queries, ["cursor=9007199254740992", "cursor=9007199254740994"])
        await transport.close()
    }

    func testMalformedEventStreamFailsWithoutReconnectLoop() async throws {
        for (mime, body) in [("application/json", "{}"), ("text/event-stream", "id: 1\ndata: {invalid\n\n")] {
            var requests = 0
            let fixture = try HTTPFixture { _ in requests += 1; return .init(headers: ["Content-Type": mime], body: body) }
            let transport = try ManagedVoiceTransport(credential: .init(origin: fixture.origin, apiKey: fixtureKey), agentID: agent, configuration: fixture.configuration)
            let stream = try await transport.events(after: "0")
            do {
                try await eventDeadline { for try await _ in stream { XCTFail("Malformed event delivered") } }
                XCTFail("Malformed stream accepted")
            } catch is APIError { /* Invalid response MIME type. */ }
            catch let error as ManagedError { XCTAssertEqual(error.code, "invalid_response") }
            XCTAssertEqual(requests, 1)
            await transport.close(); fixture.close()
        }
    }

    func testDelegationAndTranscriptUseCanonicalEscapedMarkers() throws {
        let voice = try ManagedVoiceProtocol()
        let partial = voice.realtimeMessage(.object(["type": .string("input_transcript.added"), "item": .object(["text": .string("fix ")])]))
        XCTAssertEqual(partial.effects.transcripts, [.init(speaker: "user", text: "fix ", isFinal: false)])
        let continuation = voice.realtimeMessage(.object(["type": .string("input_transcript.added"), "item": .object(["text": .string("this")])]))
        XCTAssertEqual(continuation.effects.transcripts, [.init(speaker: "user", text: "fix this", isFinal: false)])
        let completed = voice.realtimeMessage(.object(["type": .string("turn.done"), "turn": .object(["role": .string("user"), "transcript": .string("fix <x> & ship")])]))
        XCTAssertEqual(completed.effects.transcripts, [.init(speaker: "user", text: "fix <x> & ship")])
        let delegated = voice.realtimeMessage(.object(["type": .string("delegation.created"), "item": .object([
            "type": .string("delegation"), "target": .string("client"), "id": .string("delegation-1"),
            "content": .array([.object(["type": .string("input_text"), "text": .string("fix <x> & ship")])])
        ])]))
        XCTAssertEqual(delegated.delegation?.formattedInput, "<realtime_delegation>\n  <input>fix &lt;x&gt; &amp; ship</input>\n  <transcript_delta>user: fix &lt;x&gt; &amp; ship</transcript_delta>\n</realtime_delegation>")
        XCTAssertNil(voice.takeTranscriptTail())
        let ignored = voice.realtimeMessage(.object(["type": .string("delegation.created"), "item": .object(["type": .string("delegation"), "target": .string("server")])]))
        XCTAssertNil(ignored.delegation)
        _ = voice.realtimeMessage(.object(["type": .string("turn.done"), "turn": .object(["role": .string("user"), "transcript": .string("one last note")])]))
        XCTAssertTrue(voice.takeTranscriptTail()?.contains("<source>transcript_tail_flush</source>") == true)
    }

    func testUTF8ChunksHeadTailBoundsAndReconnectReplay() throws {
        let voice = try ManagedVoiceProtocol()
        let unicode = String(repeating: "a", count: 499) + String(repeating: "e\u{301}🦀", count: 200)
        let frames = voice.agentEvent(.object(["type": .string("assistant.message"), "payload": .object(["text": .string(unicode)])])).frames
        let chunks = frames.map { $0["content"].array[0]["text"].string }
        XCTAssertTrue(chunks.allSatisfy { $0.utf8.count <= 500 }); XCTAssertEqual(chunks.joined(), unicode)
        XCTAssertEqual(voice.sidebandOpened().frames, frames); voice.framesSent(frames.count)
        XCTAssertTrue(voice.sidebandOpened().frames.isEmpty)
        XCTAssertEqual((0..<6).map { _ in voice.sidebandClosed(connectedMS: 100).reconnectAfterMS }, [200, 400, 800, 1_600, 3_200, 5_000])
        XCTAssertEqual(voice.sidebandClosed(connectedMS: 30_000).reconnectAfterMS, 200)
        _ = voice.agentEvent(.object(["type": .string("run.started")]))
        _ = voice.agentEvent(.object(["type": .string("assistant.delta"), "payload": .object(["text": .string("START" + String(repeating: "x", count: 10_000) + "END")])]))
        let output = (voice.flush().frames + voice.flush(final: true).frames).map { $0["content"].array[0]["text"].string }.joined()
        XCTAssertLessThanOrEqual(output.utf8.count, 4_000); XCTAssertTrue(output.hasPrefix("START")); XCTAssertTrue(output.hasSuffix("END")); XCTAssertTrue(output.contains("output truncated"))
        let context: JSON = .object(["history": .array([.object(["role": .string("user"), "content": .array([.object(["text": .string("Continue this durable task")])])]), .object(["role": .string("system"), "content": .array([.object(["text": .string("private non-chat metadata")])])])])])
        let instructions = ManagedVoiceProtocol.instructions(context: context)
        XCTAssertTrue(instructions.contains("Continue this durable task")); XCTAssertFalse(instructions.contains("private non-chat metadata"))
        XCTAssertLessThan(instructions.utf8.count, 32_768)
        let handoff = ManagedVoiceProtocol.delegation(input: "Synthetic handoff", transcript: [.init(speaker: "user", text: "Continue our work")], tail: true)
        let voiceContext: JSON = .object(["history": .array([.object(["role": .string("user"), "content": .array([.object(["text": .string(handoff)])])])])])
        let natural = ManagedVoiceProtocol.instructions(context: voiceContext)
        XCTAssertTrue(natural.contains("Continue our work"))
        XCTAssertFalse(natural.contains("realtime_delegation")); XCTAssertFalse(natural.contains("transcript_tail_flush"))
        XCTAssertFalse(natural.contains("Synthetic handoff"))
        let frame = try XCTUnwrap(ManagedVoiceProtocol.startupContextFrames(voiceContext).first)
        XCTAssertEqual(frame["type"].string, "session.context.append")
        XCTAssertEqual(frame["channel"].string, "commentary")
        XCTAssertTrue(frame["content"].array[0]["text"].string.contains("Continue our work"))
        XCTAssertFalse(frame["content"].array[0]["text"].string.contains("realtime_delegation"))
        XCTAssertTrue(ManagedVoiceProtocol.startupContextFrames(.null).isEmpty)
        for _ in 0..<8 { XCTAssertNoThrow(try ManagedVoiceTransport.validateSession(ManagedVoiceProtocol.sessionID())) }
    }

    func testOverlappingSpeakersKeepEachPartialAndFinalTurn() throws {
        let voice = try ManagedVoiceProtocol()
        func delta(_ speaker: String, _ text: String) -> JSON {
            .object(["type": .string(speaker == "user" ? "input_transcript.added" : "output_transcript.added"), "item": .object(["text": .string(text)])])
        }
        _ = voice.realtimeMessage(delta("user", "Wait, "))
        _ = voice.realtimeMessage(delta("assistant", "I can "))
        XCTAssertEqual(voice.realtimeMessage(delta("user", "use the blue one")).effects.transcripts, [.init(speaker: "user", text: "Wait, use the blue one", isFinal: false)])
        XCTAssertEqual(voice.realtimeMessage(delta("assistant", "do that.")).effects.transcripts, [.init(speaker: "assistant", text: "I can do that.", isFinal: false)])
        _ = voice.realtimeMessage(.object(["type": .string("turn.done"), "turn": .object(["role": .string("user"), "transcript": .string("Wait, use the blue one.")])]))
        XCTAssertEqual(voice.realtimeMessage(delta("user", "Thanks")).effects.transcripts, [.init(speaker: "user", text: "Thanks", isFinal: false)])
        let tail = try XCTUnwrap(voice.takeTranscriptTail())
        XCTAssertTrue(tail.contains("user: Wait, use the blue one."))
        XCTAssertTrue(tail.contains("assistant: I can do that."))
        XCTAssertTrue(tail.contains("user: Thanks"))
    }

    func testCompletedTranscriptKeepsSpokenPrefixAndDropsInterruptedTail() throws {
        for (role, streamed, final, expected) in [
            ("assistant", " Sure thing. Starting now. One...", " thing. Starting now. One...", " Sure thing. Starting now. One..."),
            ("assistant", "One. Two. Three.", "One. Two.", "One. Two."),
            ("assistant", "cannot", "not", "not"),
            ("user", "Sure thing.", "thing.", "thing.")
        ] {
            let voice = try ManagedVoiceProtocol()
            _ = voice.realtimeMessage(.object(["type": .string(role == "user" ? "input_transcript.added" : "output_transcript.added"), "item": .object(["text": .string(streamed)])]))
            let done = voice.realtimeMessage(.object(["type": .string("turn.done"), "turn": .object(["role": .string(role), "transcript": .string(final)])]))
            XCTAssertEqual(done.effects.transcripts, [.init(speaker: role, text: expected)])
            XCTAssertTrue(try XCTUnwrap(voice.takeTranscriptTail()).contains(expected))
        }
    }

    @MainActor
    func testTranscriptsKeepStreamingDuringDurableAdmission() async throws {
        let admitted = expectation(description: "Delegation HTTP in flight")
        let queued = expectation(description: "Queued user request survives closing voice")
        let fixture = try HTTPFixture { request in
            var reply: [String: Any] = ["voice_session_id": request.json["voice_session_id"] ?? "", "operation_id": request.json["operation_id"] ?? "", "context": []]
            if request.path.hasSuffix("/delegate") {
                reply["turn_id"] = "voice-turn"; reply["route"] = "started"
                if (request.json["input"] as? String)?.contains("Do the test") == true { admitted.fulfill() }
                if (request.json["input"] as? String)?.contains("Then save this note") == true { queued.fulfill() }
            }
            return .init(body: String(data: try! JSONSerialization.data(withJSONObject: reply), encoding: .utf8)!, delay: 0.5)
        }
        defer { fixture.close() }
        let transport = try ManagedVoiceTransport(credential: .init(origin: fixture.origin, apiKey: fixtureKey), agentID: agent, configuration: fixture.configuration)
        let voice = VoiceSession()
        voice.prepareRoutingForTesting(transport: transport, agentID: agent)
        let delegation: JSON = .object(["type": .string("delegation.created"), "item": .object([
            "type": .string("delegation"), "target": .string("client"), "id": .string("one-delegation"),
            "content": .array([.object(["type": .string("input_text"), "text": .string("Do the test")])])])])
        try voice.receiveRealtimeForTesting(delegation)
        await fulfillment(of: [admitted], timeout: 2)
        try voice.receiveRealtimeForTesting(delegation) // replay cannot route again
        for (type, text) in [("input_transcript.added", "And keep listening"), ("output_transcript.added", "I am here")] {
            try voice.receiveRealtimeForTesting(.object(["type": .string(type), "item": .object(["text": .string(text)])]))
        }
        XCTAssertTrue(voice.isWorking)
        XCTAssertEqual(voice.transcripts.map(\.text), ["And keep listening", "I am here"])
        try voice.receiveRealtimeForTesting(.object(["type": .string("delegation.created"), "item": .object([
            "type": .string("delegation"), "target": .string("client"), "id": .string("next-delegation"),
            "content": .array([.object(["type": .string("input_text"), "text": .string("Then save this note")])])])]))
        voice.stop(); await voice.finishStopping()
        await fulfillment(of: [queued], timeout: 2)
        XCTAssertEqual(voice.phase, .ended)
        XCTAssertTrue(voice.transcripts.isEmpty, "A late admission must not restore the ended conversation")
    }
}

private func eventDeadline<T: Sendable>(_ operation: @escaping @Sendable () async throws -> T) async throws -> T {
    try await withThrowingTaskGroup(of: T.self) { group in
        group.addTask { try await operation() }
        group.addTask { try await Task.sleep(for: .seconds(3)); throw URLError(.timedOut) }
        defer { group.cancelAll() }
        return try await group.next()!
    }
}
