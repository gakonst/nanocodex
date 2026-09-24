import XCTest
import InboxCore
@testable import NanocodexVoice

final class ElevenLabsTests: XCTestCase {
    func testCloneWithoutVoiceIDIsNotReportedAsSuccessOrRetried() async throws {
        var requests = 0
        let fixture = try HTTPFixture { _ in
            requests += 1
            return FixtureReply(status: 201, body: "{}")
        }
        defer { fixture.close() }
        let file = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString + ".wav")
        try Data("synthetic audio".utf8).write(to: file)
        defer { try? FileManager.default.removeItem(at: file) }
        let client = try ElevenLabs(configuration: .init(baseURL: URL(string: fixture.origin)!, apiKey: fixtureKey, agentID: "019d2f5d-7491-8000-8000-000000000001"), urlConfiguration: fixture.configuration)
        do {
            _ = try await client.clone(name: "Synthetic", files: [file], consent: true)
            XCTFail("Missing voice ID cannot select a clone")
        } catch {
            XCTAssertTrue(error.localizedDescription.contains("Refresh"))
        }
        XCTAssertEqual(requests, 1)
        XCTAssertTrue(FileManager.default.fileExists(atPath: file.path), "Uncertain uploads must preserve the sample")
    }
    func testOnlyFinalCaptionSynthesizesOnceWithWholeResponse() {
        var captions = VoiceSpeechCaptions()
        XCTAssertNil(captions.consume(.init(speaker: "assistant", text: "Hello", isFinal: false, id: 1)))
        XCTAssertNil(captions.consume(.init(speaker: "assistant", text: "Hello. More", isFinal: false, id: 1)))
        let complete = "Hello. More words. Another sentence!"
        XCTAssertEqual(captions.consume(.init(speaker: "assistant", text: complete, id: 1)), complete)
        XCTAssertNil(captions.consume(.init(speaker: "assistant", text: complete, id: 1)))
        XCTAssertNil(captions.consume(.init(speaker: "assistant", text: complete + " Late revision.", id: 1)))
        XCTAssertNil(captions.consume(.init(speaker: "user", text: "User speech", id: 2)))
        XCTAssertEqual(captions.consume(.init(speaker: "assistant", text: " Next response ", id: 2)), "Next response")
        XCTAssertNil(captions.consume(.init(speaker: "assistant", text: "Old response", id: 1)))
    }
    func testInterruptSuppressesPartialCaptionAndItsLateFinal() {
        var captions = VoiceSpeechCaptions()
        XCTAssertNil(captions.consume(.init(speaker: "assistant", text: "Sentence one. Sentence two.", isFinal: false, id: 1)))
        captions.interrupt()
        let late = ManagedVoiceTranscript(speaker: "assistant", text: "Sentence one. Sentence two. Late final", id: 1)
        XCTAssertNil(captions.consume(late))
        XCTAssertTrue(captions.isSuppressed(late))
        XCTAssertEqual(captions.consume(.init(speaker: "assistant", text: "New response", id: 2)), "New response")
        captions.interrupt()
        XCTAssertNil(captions.consume(.init(speaker: "assistant", text: "New response repeated", id: 2)))
    }
    func testCloneRequiresConsentAndReturnsVerificationWithoutRetry() async throws {
        var requests = 0
        let fixture = try HTTPFixture { request in
            requests += 1
            XCTAssertEqual(request.path, "/api/voice/elevenlabs/voices")
            XCTAssertTrue(request.headers["content-type"]?.hasPrefix("multipart/form-data; boundary=") == true)
            let body = String(decoding: request.body, as: UTF8.self)
            XCTAssertTrue(body.contains("name=\"consent\"\r\n\r\ntrue"))
            XCTAssertTrue(body.contains("name=\"files\"; filename=\"sample-0.wav\""))
            XCTAssertFalse(body.contains("private-sample"))
            return FixtureReply(status: 201, body: "{\"voice_id\":\"synthetic_clone\",\"requires_verification\":true}")
        }
        defer { fixture.close() }
        let file = FileManager.default.temporaryDirectory.appendingPathComponent("private-sample-" + UUID().uuidString + ".wav")
        try Data("synthetic wave fixture".utf8).write(to: file)
        defer { try? FileManager.default.removeItem(at: file) }
        let client = try ElevenLabs(configuration: .init(baseURL: URL(string: fixture.origin)!, apiKey: fixtureKey, agentID: "019d2f5d-7491-8000-8000-000000000001"), urlConfiguration: fixture.configuration)
        do { _ = try await client.clone(name: "Synthetic", files: [file], consent: false); XCTFail("Consent is required") } catch {}
        XCTAssertEqual(requests, 0)
        let result = try await client.clone(name: "Synthetic", files: [file], consent: true)
        XCTAssertTrue(result["requires_verification"].bool)
        XCTAssertEqual(requests, 1)
    }
    @MainActor func testCancelRevokesPendingAudioAndQueuedRequests() async throws {
        let began = expectation(description: "first synthesis began")
        let player = VoiceSpeechPlayer()
        player.enqueue(audio: {
            began.fulfill()
            try await Task.sleep(for: .seconds(5))
            return Data()
        }, onError: { _ in XCTFail("Cancelled audio must not produce an error") })
        player.enqueue(audio: { XCTFail("Queued synthesis must be revoked"); return Data() }, onError: { _ in XCTFail("Cancelled queue must not produce an error") })
        await fulfillment(of: [began], timeout: 1)
        player.cancel()
        try await Task.sleep(for: .milliseconds(30))
        XCTAssertEqual(player.level, 0)
    }

}
