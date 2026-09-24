import Foundation
import XCTest
@testable import InboxCore

final class QuickVoiceCaptureGateTests: XCTestCase {
    func testSilenceWaitsForFinalAndSubmitsOnlyOnce() {
        var gate = QuickVoiceCaptureGate()
        let token = gate.begin()
        XCTAssertNil(gate.completed("a partial", token: token, isFinal: false))
        XCTAssertTrue(gate.accepts(token)) // endAudio/grace still permits a final result
        XCTAssertEqual(gate.completed("a final", token: token, isFinal: true), "a final")
        XCTAssertNil(gate.completed("a duplicate", token: token, isFinal: true))
    }
    func testInterruptionAndTimeoutRejectLateFinals() {
        var gate = QuickVoiceCaptureGate()
        let token = gate.begin()
        gate.cancel() // interruption, timeout, permission failure, or explicit Cancel
        XCTAssertNil(gate.completed("late final", token: token, isFinal: true))
        XCTAssertFalse(gate.accepts(token))
    }
    func testLanguageRestartRejectsPreviousRecognizer() {
        var gate = QuickVoiceCaptureGate()
        let english = gate.begin()
        let greek = gate.begin()
        XCTAssertNil(gate.completed("old English result", token: english, isFinal: true))
        XCTAssertEqual(gate.completed("Νέα εργασία", token: greek, isFinal: true), "Νέα εργασία")
    }
    func testEmptyFinalCannotSubmit() {
        var gate = QuickVoiceCaptureGate()
        let token = gate.begin()
        XCTAssertNil(gate.completed("  \n", token: token, isFinal: true))
    }
}
