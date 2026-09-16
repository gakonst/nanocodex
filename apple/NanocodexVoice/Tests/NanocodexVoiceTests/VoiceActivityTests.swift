import XCTest
@testable import NanocodexVoice

@MainActor
final class VoiceActivityTests: XCTestCase {
    func testPresentationActivityPreservesSafetyPrecedence() {
        let cases: [(VoiceSession.Phase, Bool, Bool, Double, Bool, VoiceSession.Activity)] = [
            (.idle, true, true, 1, true, .ready),
            (.ended, true, true, 1, true, .ready),
            (.connecting, true, true, 1, true, .connecting),
            (.failed, true, true, 1, true, .failed),
            (.active, true, true, 1, true, .reconnecting),
            (.active, true, false, 1, true, .muted),
            (.active, false, false, 0.016, true, .speaking),
            (.active, false, false, 0.015, true, .working),
            (.active, false, false, 0, false, .listening),
        ]

        for (phase, muted, reconnecting, output, working, expected) in cases {
            XCTAssertEqual(
                VoiceSession.activity(phase: phase, isMuted: muted, isReconnecting: reconnecting,
                                      outputLevel: output, isWorking: working),
                expected
            )
        }
    }

    func testInitialActivityAndHumanStatusAreReady() {
        let voice = VoiceSession()
        XCTAssertEqual(voice.activity, .ready)
        XCTAssertEqual(voice.status, "Ready to talk")
    }
}
