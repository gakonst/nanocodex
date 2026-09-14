import XCTest
import Foundation
@testable import InboxCore

// HandTaskExecution.swift is linked to the app source so these tests exercise
// the same observer used by iOS expiration, without duplicating its lifecycle.
@MainActor
final class HandTaskExecutionTests: XCTestCase {
    func testExpirationEndsOnlyLocalObservationAndKeepsAcceptedTurnRunning() async throws {
        let requests = RequestLog()
        let fixture = try HTTPFixture { request in
            requests.append(request.path)
            return .init(body: #"{"turn_id":"turn","state":"accepted"}"#)
        }
        defer { fixture.close() }
        let credential = try AccountCredential(origin: fixture.origin, apiKey: fixtureKey)
        let client = ManagedClient(credential: credential, configuration: fixture.configuration)
        defer { client.close() }
        let observing = expectation(description: "accepted turn is being observed")
        let progress = Progress(totalUnitCount: 1)
        let owner = HandTaskExecution(changed: {}, failed: { XCTFail($0) })
        let work = owner.start(id: "turn", title: "Remote work", progress: progress, runtimeProvided: true) { _ in
            _ = try await client.command(.init(agentID: "agent", input: "continue remotely", kind: .followUp, requestID: "turn"))
            observing.fulfill()
            try await Task.sleep(for: .seconds(60))
            return "unexpected local completion"
        }
        await fulfillment(of: [observing], timeout: 3)
        // This is the exact method called by the iOS expiration handler.
        owner.endObservation(id: "turn")
        do { _ = try await work.value; XCTFail("local observation remained alive") }
        catch { XCTAssertTrue(error is CancellationError) }
        XCTAssertEqual(progress.localizedAdditionalDescription, "Paused")
        XCTAssertFalse(owner.hasBackgroundRuntime)
        let state = try await client.turn(agentID: "agent", turnID: "turn")
        XCTAssertEqual(state["state"].string, "accepted")
        XCTAssertEqual(requests.values, ["/v1/agents/agent/turns", "/v1/agents/agent/turns/turn"])
    }

    func testLiveRemoteTurnSurvivesObservationExpiryAndClientClose() async throws {
        let env = ProcessInfo.processInfo.environment
        guard env["NANOCODEX_OBSERVER_LIVE"] == "1" else {
            throw XCTSkip("Opt in with NANOCODEX_OBSERVER_LIVE=1 and NC_API_KEY")
        }
        let credential = try AccountCredential(
            origin: env["NANOCODEX_MANAGED_URL"] ?? "https://nanocodex.gakonst.workers.dev",
            apiKey: try XCTUnwrap(env["NANOCODEX_API_KEY"] ?? env["NC_API_KEY"]))
        let client = ManagedClient(credential: credential)
        let reconnected = ManagedClient(credential: credential)
        let agentID = try await client.create(requestID: UUID().uuidString)
        addTeardownBlock {
            _ = try await reconnected.json(path: "/v1/agents/" + agentID, method: "DELETE")
            reconnected.close(); client.close()
        }
        let turnID = UUID().uuidString, marker = "OBSERVER_EXPIRED_CLOUD_CONTINUED"
        let receipt = try await client.command(.init(agentID: agentID,
            input: "This is a self-contained lifecycle test. Do not use tools, memory or external services. Explain why merge sort is O(n log n), checking the recurrence carefully, in about 500 words. End with " + marker,
            kind: .followUp, requestID: turnID))
        XCTAssertEqual(receipt["state"].string, "accepted")
        let admitted = try await client.turn(agentID: agentID, turnID: turnID)
        let cursor = try XCTUnwrap(Cursor(rawValue: admitted["accepted_cursor"].string))
        let opened = expectation(description: "real managed event stream opened")
        let owner = HandTaskExecution(changed: {}, failed: { XCTFail($0) })
        let work = owner.start(id: turnID, title: "Live lifecycle regression", runtimeProvided: true) { _ in
            try await client.stream(agentID, after: cursor, onOpen: { opened.fulfill() }) { _ in }
            return "stream ended"
        }
        await fulfillment(of: [opened], timeout: 30)
        owner.endObservation(id: turnID)
        client.close()
        _ = await work.result
        let deadline = Date().addingTimeInterval(180)
        var state: JSON = .null
        repeat {
            state = try await reconnected.turn(agentID: agentID, turnID: turnID)
            if ["completed", "failed", "cancelled"].contains(state["state"].string) { break }
            try await Task.sleep(for: .seconds(2))
        } while Date() < deadline
        XCTAssertEqual(state["state"].string, "completed")
        XCTAssertTrue(state["terminal"]["final_message"].string.contains(marker))
        print("Live observer expiry and client close preserved remote turn \(agentID)/\(turnID)")
    }

    func testLocalCancellationIsPausedAndRemoteCancellationIsStopped() async {
        for remote in [false, true] {
            let progress = Progress(totalUnitCount: 1)
            let owner = HandTaskExecution(changed: {}, failed: { XCTFail($0) })
            let work = owner.start(id: "turn", title: "Remote work", progress: progress, runtimeProvided: true) { _ in
                if remote { throw HandTaskError.cancelled }
                throw CancellationError()
            }
            _ = await work.result
            XCTAssertEqual(progress.localizedAdditionalDescription, remote ? "Stopped" : "Paused")
            XCTAssertFalse(owner.hasBackgroundRuntime)
        }
    }

    func testEndingAllObserversDoesNotResurrectThemAfterLateCompletion() async {
        let owner = HandTaskExecution(changed: {}, failed: { XCTFail($0) })
        let progress = Progress(totalUnitCount: 1)
        let started = expectation(description: "observer started")
        let gate = CompletionGate()
        let work = owner.start(id: "turn", title: "Remote work", progress: progress, runtimeProvided: true) { _ in
            started.fulfill()
            return await gate.wait()
        }
        await fulfillment(of: [started], timeout: 3)
        owner.endAllObservations()
        await gate.finish()
        _ = await work.result
        XCTAssertEqual(progress.localizedAdditionalDescription, "Paused")
        XCTAssertFalse(owner.hasBackgroundRuntime)
    }
}

private final class RequestLog: @unchecked Sendable {
    private let lock = NSLock()
    private var paths: [String] = []
    func append(_ path: String) { lock.lock(); defer { lock.unlock() }; paths.append(path) }
    var values: [String] { lock.lock(); defer { lock.unlock() }; return paths }
}

private actor CompletionGate {
    private var continuation: CheckedContinuation<String, Never>?
    private var finished = false
    func wait() async -> String {
        if finished { return "late result" }
        return await withCheckedContinuation { continuation = $0 }
    }
    func finish() { finished = true; continuation?.resume(returning: "late result"); continuation = nil }
}
