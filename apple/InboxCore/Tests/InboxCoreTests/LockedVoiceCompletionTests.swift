import XCTest
@testable import InboxCore

@MainActor
final class LockedVoiceCompletionTests: XCTestCase {
    private enum Failure: Error, Equatable { case transcription, delivery }

    func testSendStaysAliveUntilTerminalAdmissionAndPersistence() async throws {
        let completion = LockedVoiceCompletion()
        let entered = expectation(description: "Send entered")
        var returned = false
        let intent = Task { @MainActor in
            entered.fulfill()
            try await completion.wait()
            returned = true
        }
        await fulfillment(of: [entered])
        // Neither stopping audio nor receiving a transcript is a terminal result.
        await Task.yield()
        XCTAssertFalse(returned)
        completion.resolve(.success(()))
        try await intent.value
        XCTAssertTrue(returned)
    }

    func testRepeatedSendWaitsForSameFailure() async {
        let completion = LockedVoiceCompletion()
        let first = Task { try await completion.wait() }
        let second = Task { try await completion.wait() }
        await Task.yield()
        completion.resolve(.failure(Failure.delivery))
        for task in [first, second] {
            do { try await task.value; XCTFail("Failed delivery must not return success") }
            catch { XCTAssertEqual(error as? Failure, .delivery) }
        }
    }

    func testSynchronousFailureBeforeWaitIsRetained() async {
        let completion = LockedVoiceCompletion()
        completion.resolve(.failure(Failure.transcription))
        do { try await completion.wait(); XCTFail("Recognition failure must reach Send") }
        catch { XCTAssertEqual(error as? Failure, .transcription) }
    }

    func testCancellationCannotBeOverwrittenByLateAdmission() async {
        let completion = LockedVoiceCompletion()
        completion.resolve(.failure(CancellationError()))
        completion.resolve(.success(()))
        do { try await completion.wait(); XCTFail("Late callbacks must not replace cancellation") }
        catch { XCTAssertTrue(error is CancellationError) }
    }

    func testPreviousCaptureCannotCompleteNewCapture() async throws {
        let old = LockedVoiceCompletion()
        let current = LockedVoiceCompletion()
        var returned = false
        let intent = Task {
            try await current.wait()
            returned = true
        }
        old.resolve(.success(()))
        await Task.yield()
        XCTAssertFalse(returned)
        current.resolve(.success(()))
        try await intent.value
        XCTAssertTrue(returned)
    }
}
