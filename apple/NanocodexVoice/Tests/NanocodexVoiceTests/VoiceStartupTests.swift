import Combine
import Foundation
import InboxCore
import XCTest
@testable import NanocodexVoice

final class VoiceStartupTests: XCTestCase {
    private let agent = "11111111-1111-7111-8111-111111111111"

    private func configuration(_ origin: String) -> VoiceConfiguration {
        .init(baseURL: URL(string: origin)!, apiKey: fixtureKey, agentID: agent, voice: "spruce")
    }
    private func receipt(_ request: FixtureRequest, delay: Double = 0) -> FixtureReply {
        .init(body: String(data: try! JSONSerialization.data(withJSONObject: [
            "voice_session_id": request.json["voice_session_id"]!, "operation_id": request.json["operation_id"]!, "context": []
        ]), encoding: .utf8)!, delay: delay)
    }
    @MainActor private func settles(_ voice: VoiceSession, within seconds: Double) async throws {
        let deadline = ContinuousClock.now.advanced(by: .seconds(seconds))
        while voice.phase == .connecting, ContinuousClock.now < deadline { try await Task.sleep(for: .milliseconds(5)) }
        XCTAssertEqual(voice.phase, .failed)
    }

    @MainActor func testWholeDeadlinePublishesFailureWithoutWaitingForUncooperativeConfiguration() async throws {
        let voice = VoiceSession()
        var late: CheckedContinuation<VoiceConfiguration, Error>?
        let entered = expectation(description: "Configuration entered")
        var published: [VoiceSession.Phase] = []
        let observer = voice.$phase.sink { published.append($0) }
        defer { observer.cancel(); voice.stop() }
        let began = ContinuousClock.now
        voice.startPreparingForTesting(timeout: .milliseconds(100)) {
            try await withCheckedThrowingContinuation { continuation in late = continuation; entered.fulfill() }
        }
        await fulfillment(of: [entered], timeout: 1)
        try await settles(voice, within: 1)
        XCTAssertLessThan(began.duration(to: .now), .seconds(1))
        XCTAssertEqual(published.last, .failed, "The observable UI leaves Connecting before the callback resumes")
        XCTAssertEqual(voice.status, "Voice paused")
        XCTAssertTrue(voice.errorMessage?.contains("too long") == true)
        XCTAssertFalse(voice.hasNativePeerForTesting)

        voice.startPreparingForTesting(timeout: .seconds(1)) {
            throw ManagedError(code: "fixture_denied", message: "Second attempt was rejected.")
        }
        try await settles(voice, within: 1)
        late?.resume(returning: configuration("https://late-voice.invalid")); late = nil
        try await Task.sleep(for: .milliseconds(30))
        XCTAssertEqual(voice.errorMessage, "Second attempt was rejected.")
        XCTAssertEqual(voice.phase, .failed)
        XCTAssertFalse(voice.hasNativePeerForTesting, "A late configuration cannot prepare or revive a peer")
    }

    @MainActor func testAdmissionAndEventFailuresSurfaceWhileMediaHTTPIsStillPending() async throws {
        for failAdmission in [true, false] {
        let callStarted = expectation(description: "Media HTTP started")
        let stopped = expectation(description: "Failed session cleaned up")
        let fixture = try HTTPFixture { request in
            if request.path.hasSuffix("/calls") {
                let session = request.json["session"] as? [String: Any]
                let audio = session?["audio"] as? [String: Any]
                XCTAssertEqual((audio?["output"] as? [String: Any])?["voice"] as? String, "spruce")
                callStarted.fulfill()
                return .init(status: 201, headers: ["Content-Type": "application/sdp", "x-nanocodex-realtime-location": "https://provider.invalid/v1/realtime/calls/rtc_fixture"], body: "v=0\r\nlate-answer", delay: 3)
            }
            if request.path.hasSuffix("/start") {
                return failAdmission ? .init(status: 403, body: #"{"error":"forbidden","message":"Admission denied by fixture."}"#, delay: 0.6) : self.receipt(request)
            }
            if request.path.hasSuffix("/stop") { stopped.fulfill(); return self.receipt(request) }
            if request.path.hasSuffix("/events") { return .init(headers: ["Content-Type": "text/event-stream"], body: ": keepalive\n\n", delay: 2) }
            return failAdmission ? .init(body: #"{"latest_event_cursor":"0"}"#) : .init(status: 403, delay: 0.6)
        }
        defer { fixture.close() }
        let transport = try ManagedVoiceTransport(credential: .init(origin: fixture.origin, apiKey: fixtureKey), agentID: agent, configuration: fixture.configuration)
        let voice = VoiceSession(), began = ContinuousClock.now
        voice.startPreparingForTesting(timeout: .seconds(5), transport: transport) { self.configuration(fixture.origin) }
        await fulfillment(of: [callStarted], timeout: 2)
        try await settles(voice, within: 1.5)
        XCTAssertLessThan(began.duration(to: .now), .seconds(2))
        XCTAssertEqual(voice.errorMessage, failAdmission ? "Admission denied by fixture." : APIError.http(403).localizedDescription)
        XCTAssertFalse(voice.hasNativePeerForTesting)
        await voice.finishStopping()
        await fulfillment(of: [stopped], timeout: 1)
        }
    }

    @MainActor func testWholeDeadlineCancelsPendingAdmissionWithoutRetryAndStopRemainsOrdered() async throws {
        let admitted = expectation(description: "Start request sent")
        var lifecycle: [FixtureRequest] = []
        let fixture = try HTTPFixture { request in
            if request.path.hasSuffix("/start") { lifecycle.append(request); admitted.fulfill(); return self.receipt(request, delay: 3) }
            if request.path.hasSuffix("/stop") { lifecycle.append(request); return self.receipt(request) }
            if request.path.hasSuffix("/calls") {
                return .init(status: 201, headers: ["Content-Type": "application/sdp", "x-nanocodex-realtime-location": "https://provider.invalid/v1/realtime/calls/rtc_fixture"], body: "v=0\r\nlate-answer", delay: 3)
            }
            if request.path.hasSuffix("/events") { return .init(headers: ["Content-Type": "text/event-stream"], body: ": keepalive\n\n", delay: 3) }
            return .init(body: #"{"latest_event_cursor":"0"}"#)
        }
        defer { fixture.close() }
        let transport = try ManagedVoiceTransport(credential: .init(origin: fixture.origin, apiKey: fixtureKey), agentID: agent, configuration: fixture.configuration)
        let voice = VoiceSession(), began = ContinuousClock.now
        voice.startPreparingForTesting(timeout: .milliseconds(250), transport: transport) { self.configuration(fixture.origin) }
        await fulfillment(of: [admitted], timeout: 1)
        try await settles(voice, within: 1)
        await voice.finishStopping()
        XCTAssertLessThan(began.duration(to: .now), .seconds(1.5), "Cleanup must not await the unrelated pending media callback")
        XCTAssertEqual(lifecycle.map { $0.path.components(separatedBy: "/").last! }, ["start", "stop"])
        XCTAssertEqual(lifecycle[0].json["voice_session_id"] as? String, lifecycle[1].json["voice_session_id"] as? String)
        XCTAssertNotEqual(lifecycle[0].json["operation_id"] as? String, lifecycle[1].json["operation_id"] as? String)
        XCTAssertFalse(voice.hasNativePeerForTesting)
        XCTAssertTrue(voice.errorMessage?.contains("too long") == true)
    }

    @MainActor func testDeadlineEscapesPriorCleanupWithoutCancellingItsDurableRequest() async throws {
        let delegated = expectation(description: "Previous request admitted")
        let previousStopped = expectation(description: "Previous session cleaned up after durable request")
        var previousSession: String?
        var startRequests = 0
        let fixture = try HTTPFixture { request in
            if request.path.hasSuffix("/delegate") {
                previousSession = request.json["voice_session_id"] as? String; delegated.fulfill()
                return .init(body: String(data: try! JSONSerialization.data(withJSONObject: [
                    "voice_session_id": request.json["voice_session_id"]!, "operation_id": request.json["operation_id"]!,
                    "route": "started", "turn_id": "previous-turn"
                ]), encoding: .utf8)!, delay: 1.2)
            }
            if request.path.hasSuffix("/stop") {
                if request.json["voice_session_id"] as? String == previousSession { previousStopped.fulfill() }
                return self.receipt(request)
            }
            if request.path.hasSuffix("/start") { startRequests += 1; return self.receipt(request) }
            if request.path.hasSuffix("/calls") {
                return .init(status: 201, headers: ["Content-Type": "application/sdp", "x-nanocodex-realtime-location": "https://provider.invalid/v1/realtime/calls/rtc_fixture"], body: "v=0\r\nlate-answer", delay: 3)
            }
            if request.path.hasSuffix("/events") { return .init(headers: ["Content-Type": "text/event-stream"], body: ": keepalive\n\n", delay: 3) }
            return .init(body: #"{"latest_event_cursor":"0"}"#)
        }
        defer { fixture.close() }
        let credential = try AccountCredential(origin: fixture.origin, apiKey: fixtureKey)
        let oldTransport = try ManagedVoiceTransport(credential: credential, agentID: agent, configuration: fixture.configuration)
        let voice = VoiceSession()
        voice.prepareRoutingForTesting(transport: oldTransport, agentID: agent)
        try voice.receiveRealtimeForTesting(.object(["type": .string("delegation.created"), "item": .object([
            "type": .string("delegation"), "target": .string("client"), "id": .string("previous-request"),
            "content": .array([.object(["type": .string("input_text"), "text": .string("Keep this durable request")])])])]))
        await fulfillment(of: [delegated], timeout: 1)
        voice.stop()
        let newTransport = try ManagedVoiceTransport(credential: credential, agentID: agent, configuration: fixture.configuration)
        let began = ContinuousClock.now
        voice.startPreparingForTesting(timeout: .milliseconds(100), transport: newTransport) { self.configuration(fixture.origin) }
        try await settles(voice, within: 1)
        await voice.finishStopping()
        XCTAssertLessThan(began.duration(to: .now), .seconds(0.8), "An abandoned admission must escape its prior-cleanup wait")
        XCTAssertEqual(startRequests, 0, "Do not start a new durable session before the prior cleanup finishes")
        await fulfillment(of: [previousStopped], timeout: 2)
        XCTAssertEqual(voice.phase, .failed)
        XCTAssertFalse(voice.hasNativePeerForTesting)
    }

    @MainActor func testAnotherConversationStartsButReturningWaitsForPreviousStop() async throws {
        let previousStop = expectation(description: "Previous stop began")
        let nextStart = expectation(description: "Independent conversation admitted")
        let returningStart = expectation(description: "Original conversation admitted after its stop")
        let otherAgent = "22222222-2222-7222-8222-222222222222"
        var originalSession: String?
        let fixture = try HTTPFixture { request in
            if request.path.hasSuffix("/stop") {
                if originalSession == nil {
                    originalSession = request.json["voice_session_id"] as? String
                    previousStop.fulfill(); return self.receipt(request, delay: 1.2)
                }
                return self.receipt(request)
            }
            if request.path.hasSuffix("/start") {
                if request.path.contains(otherAgent) { nextStart.fulfill() }
                else { returningStart.fulfill() }
                return self.receipt(request)
            }
            if request.path.hasSuffix("/calls") {
                return .init(status: 201, headers: ["Content-Type": "application/sdp", "x-nanocodex-realtime-location": "https://provider.invalid/v1/realtime/calls/rtc_fixture"], body: "v=0\r\nlate-answer", delay: 3)
            }
            if request.path.hasSuffix("/events") { return .init(headers: ["Content-Type": "text/event-stream"], body: ": keepalive\n\n", delay: 3) }
            return .init(body: #"{"latest_event_cursor":"0"}"#)
        }
        defer { fixture.close() }
        let credential = try AccountCredential(origin: fixture.origin, apiKey: fixtureKey)
        let oldTransport = try ManagedVoiceTransport(credential: credential, agentID: agent, configuration: fixture.configuration)
        let voice = VoiceSession()
        voice.prepareRoutingForTesting(transport: oldTransport, agentID: agent)
        voice.stop()
        await fulfillment(of: [previousStop], timeout: 1)
        let newTransport = try ManagedVoiceTransport(credential: credential, agentID: otherAgent, configuration: fixture.configuration)
        let began = ContinuousClock.now
        voice.startPreparingForTesting(timeout: .seconds(3), transport: newTransport) {
            .init(baseURL: URL(string: fixture.origin)!, apiKey: fixtureKey, agentID: otherAgent)
        }
        await fulfillment(of: [nextStart], timeout: 2)
        let elapsed = began.duration(to: .now)
        print("VOICE_FIXTURE independent_admission_seconds=\(elapsed)")
        voice.stop()
        await voice.finishStopping()
        XCTAssertLessThan(elapsed, .seconds(0.8), "Another agent must not wait for the previous agent's stop receipt")

        // Finishing B's cleanup must not forget A's still-pending stop.
        let returningTransport = try ManagedVoiceTransport(credential: credential, agentID: agent, configuration: fixture.configuration)
        let returning = ContinuousClock.now
        voice.startPreparingForTesting(timeout: .seconds(3), transport: returningTransport) { self.configuration(fixture.origin) }
        await fulfillment(of: [returningStart], timeout: 2)
        let returningElapsed = returning.duration(to: .now)
        print("VOICE_FIXTURE returning_admission_seconds=\(returningElapsed)")
        voice.stop()
        await voice.finishStopping()
        XCTAssertGreaterThan(returningElapsed, .seconds(0.8), "Returning to the original agent must await its still-pending stop")
    }
}
