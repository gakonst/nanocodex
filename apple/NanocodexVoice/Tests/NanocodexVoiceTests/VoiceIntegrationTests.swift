import AVFoundation
import Foundation
import XCTest
import InboxCore
@testable import NanocodexVoice

final class VoiceIntegrationTests: XCTestCase {
    /// Opt-in native WebRTC and authenticated data-channel evidence. No
    /// microphone permission, capture, or synthetic speech is involved.
    @MainActor
    func testNativeManagedVoiceConnectsAndStops() async throws {
        let environment = ProcessInfo.processInfo.environment
        guard environment["NANOCODEX_VOICE_LIVE"] == "1", let key = environment["NC_API_KEY"] else {
            throw XCTSkip("Set NANOCODEX_VOICE_LIVE=1 and NC_API_KEY for live voice evidence.")
        }
        let credential = try AccountCredential(origin: environment["NANOCODEX_MANAGED_URL"] ?? "https://nanocodex.gakonst.workers.dev", apiKey: key)
        let client = ManagedClient(credential: credential)
        defer { client.close() }
        let agentID = try await client.create(requestID: UUID().uuidString)
        print("Native voice validation agent: \(agentID)")
        fflush(stdout)
        let microphoneBefore = AVCaptureDevice.authorizationStatus(for: .audio)
        let voice = VoiceSession()
        do {
            let configuration = VoiceConfiguration(baseURL: try XCTUnwrap(URL(string: credential.origin)), apiKey: key,
                                                   agentID: agentID, conversationTitle: "Native voice validation", voice: "spruce")
            let began = ContinuousClock.now
            voice.startReceivingForTesting(configuration: configuration)
            let deadline = began.advanced(by: .seconds(50))
            while voice.phase == .connecting, ContinuousClock.now < deadline {
                try await Task.sleep(for: .milliseconds(50))
            }
            guard voice.phase == .active else {
                throw VoiceEvidenceFailure(message: voice.errorMessage ?? "Native media and data channel did not connect within 50 seconds.")
            }
            XCTAssertTrue(voice.hasNativePeerForTesting)
            XCTAssertEqual(voice.conversationID, agentID)
            XCTAssertEqual(voice.audioBytesSent, 0)
            print("Native voice connected with data channel in \(began.duration(to: .now)).")
            XCTAssertEqual(AVCaptureDevice.authorizationStatus(for: .audio), microphoneBefore)
            voice.toggleMute(); XCTAssertTrue(voice.isMuted)
            voice.toggleMute(); XCTAssertFalse(voice.isMuted)
            // Explicit context remains supported independently of startup.
            try voice.appendContext("The sample project is Juniper. Wait for me to speak before replying.")
            // A no-op update proves the control channel accepts writes and
            // returns protocol acknowledgements without changing call behavior.
            try voice.sendRealtimeForTesting(.object(["type": .string("session.update"), "session": .object([:])]))
            let eventDeadline = ContinuousClock.now.advanced(by: .seconds(10))
            while !voice.receivedRealtimeTypesForTesting.contains("session.updated"), voice.isEngaged, ContinuousClock.now < eventDeadline {
                try await Task.sleep(for: .milliseconds(50))
            }
            XCTAssertTrue(voice.receivedRealtimeTypesForTesting.contains("session.started"))
            XCTAssertTrue(voice.receivedRealtimeTypesForTesting.contains("session.updated"), "Control writes and acknowledgements must use the same WebRTC data channel")
            XCTAssertEqual(voice.phase, .active)
            try await Task.sleep(for: .seconds(2))
            XCTAssertEqual(voice.phase, .active, "Explicit background context must be accepted by the real provider")
            XCTAssertFalse(voice.receivedRealtimeTypesForTesting.contains("output_transcript.added"), "Background context must not prompt unsolicited speech")

            let bytesBeforeSpeech = voice.audioBytesReceived
            let speechBegan = ContinuousClock.now
            try voice.speak("Voice is ready.")
            let speechDeadline = speechBegan.advanced(by: .seconds(15))
            while voice.audioBytesReceived == bytesBeforeSpeech, voice.isEngaged, ContinuousClock.now < speechDeadline {
                try await Task.sleep(for: .milliseconds(20))
            }
            XCTAssertGreaterThan(voice.audioBytesReceived, bytesBeforeSpeech, "Explicit speech must reach native WebRTC")
            print("Native speech request to first observed audio bytes: \(speechBegan.duration(to: .now)).")

            let stoppedAt = ContinuousClock.now
            voice.stop()
            XCTAssertLessThan(stoppedAt.duration(to: .now), .seconds(1))
            XCTAssertFalse(voice.hasNativePeerForTesting)
            XCTAssertFalse(voice.isEngaged)
            XCTAssertEqual(voice.inputLevel, 0); XCTAssertEqual(voice.outputLevel, 0)
            await voice.finishStopping()

            // Late SDP/service completions must not revive a stopped call.
            voice.startReceivingForTesting(configuration: configuration)
            let preparingDeadline = ContinuousClock.now.advanced(by: .seconds(3))
            while !voice.hasNativePeerForTesting, voice.phase == .connecting, ContinuousClock.now < preparingDeadline {
                try await Task.sleep(for: .milliseconds(10))
            }
            voice.stop(); await voice.finishStopping()
            try await Task.sleep(for: .milliseconds(350))
            XCTAssertFalse(voice.hasNativePeerForTesting)
            XCTAssertEqual(voice.phase, .ended)
            XCTAssertEqual(voice.audioBytesReceived, 0)
        } catch {
            voice.stop(); await voice.finishStopping()
            try? await deleteAgent(agentID, client: client)
            throw error
        }
        try await deleteAgent(agentID, client: client)
    }

    private func deleteAgent(_ id: String, client: ManagedClient) async throws {
        let path = try ManagedClient.agentPath(id)
        for attempt in 0..<5 {
            do { _ = try await client.json(path: path, method: "DELETE"); return }
            catch APIError.http(404) { return }
            catch APIError.http(503) where attempt < 4 { try await Task.sleep(for: .seconds(2)) }
        }
        throw VoiceEvidenceFailure(message: "Validation agent cleanup remains pending: " + id)
    }
}

private struct VoiceEvidenceFailure: LocalizedError {
    let message: String
    var errorDescription: String? { message }
}
