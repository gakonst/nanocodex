import XCTest
import Foundation
@testable import InboxCore

#if !os(Linux)
final class StreamRecoveryTests: XCTestCase {
    private let agent = "owned-agent"

    func testHeartbeatOnlyStreamReopensFromDeliveredCursorToReplayPersistedCompletion() async throws {
        var queries: [String] = [], stateReads = 0
        let completion = "id: 9007199254740994\ndata: {\"type\":\"turn_completed\",\"id\":\"owned-turn\",\"final_message\":\"Persisted result\"}\n\n"
        let fixture = try HTTPFixture { request in
            if request.path.hasSuffix("/events") {
                queries.append(request.query ?? "")
                return .init(headers: ["Content-Type": "text/event-stream"], body: queries.count == 1 ? ": keepalive\n\n" : completion,
                             streaming: queries.count == 1)
            }
            stateReads += 1
            return .init(body: #"{"agent_id":"owned-agent","latest_event_cursor":"9007199254740994"}"#)
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try .init(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        let received = FrameRecorder(cursor: Cursor(rawValue: "9007199254740993")!)
        do {
            try await bounded {
                try await client.stream(self.agent, after: await received.cursor, idleCheckInterval: .milliseconds(20)) { await received.append($0) }
            }
            XCTFail("Heartbeat-only stale stream remained attached")
        } catch let error as URLError { XCTAssertEqual(error.code, .networkConnectionLost) }
        let beforeReplay = await received.values()
        XCTAssertEqual(beforeReplay, [])
        try await client.stream(agent, after: await received.cursor, idleCheckInterval: .milliseconds(20)) { await received.append($0) }
        let afterReplay = await received.values()
        XCTAssertEqual(afterReplay, ["turn_completed:9007199254740994:Persisted result"])
        XCTAssertEqual(queries, ["cursor=9007199254740993", "cursor=9007199254740993"])
        XCTAssertGreaterThan(stateReads, 0)
    }

    func testHealthyIdleUnavailableAndMismatchedStateKeepTheStreamOpenAndCancelPromptly() async throws {
        for (status, body) in [
            (200, #"{"agent_id":"owned-agent","latest_event_cursor":"1"}"#),
            (503, #"{"error":"unavailable"}"#),
            (200, #"{"agent_id":"another-agent","latest_event_cursor":"999"}"#),
            (200, #"{"agent_id":"owned-agent","latest_event_cursor":"invalid"}"#),
            (200, "{invalid"),
        ] {
            let reads = expectation(description: "Idle stream reconciliation reads state")
            reads.expectedFulfillmentCount = 2
            var stateReads = 0
            let fixture = try HTTPFixture { request in
                if request.path.hasSuffix("/events") {
                    return .init(headers: ["Content-Type": "text/event-stream"], body: ": keepalive\n\n", streaming: true)
                }
                stateReads += 1
                if stateReads <= 2 { reads.fulfill() }
                return .init(status: status, body: body)
            }
            defer { fixture.close() }
            let client = ManagedClient(credential: try .init(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
            defer { client.close() }
            let reader = Task { try await client.stream(agent, after: Cursor(rawValue: "1")!, idleCheckInterval: .milliseconds(20)) { _ in } }
            await fulfillment(of: [reads], timeout: 0.5)
            reader.cancel()
            do { try await bounded { try await reader.value }; XCTFail("Cancelled stream completed normally") }
            catch is CancellationError { }
            catch let error as URLError { XCTAssertEqual(error.code, .cancelled) }
        }
    }

    func testProgressDuringStateReadPreventsStaleReconnect() async throws {
        let progressed = expectation(description: "Completion delivered while state read waits")
        let fixture = try HTTPFixture { request in
            if request.path.hasSuffix("/events") {
                return .init(headers: ["Content-Type": "text/event-stream"], body: ": keepalive\n\n", streaming: true,
                    chunks: [(delay: 0.04, body: "id: 2\ndata: {\"type\":\"turn_completed\",\"id\":\"owned-turn\"}\n\n")])
            }
            return .init(body: #"{"agent_id":"owned-agent","latest_event_cursor":"2"}"#, delay: 0.05)
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try .init(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        let reader = Task { try await client.stream(agent, after: Cursor(rawValue: "1")!, idleCheckInterval: .milliseconds(20)) { frame in
            if frame.event != nil { progressed.fulfill() }
        } }
        await fulfillment(of: [progressed], timeout: 0.5)
        try await Task.sleep(for: .milliseconds(100))
        reader.cancel()
        do { try await bounded { try await reader.value }; XCTFail("Cancelled stream completed normally") }
        catch is CancellationError { }
        catch let error as URLError { XCTAssertEqual(error.code, .cancelled) }
    }
}

private actor FrameRecorder {
    var cursor: Cursor
    private var events: [String] = []
    init(cursor: Cursor) { self.cursor = cursor }
    func append(_ frame: SSEFrame) {
        if let event = frame.event { events.append("\(event.type):\(event.cursor.rawValue):\(event.data["final_message"].string)") }
        if let next = frame.cursor { cursor = max(cursor, next) }
    }
    func values() -> [String] { events }
}

private func bounded<T: Sendable>(_ operation: @escaping @Sendable () async throws -> T) async throws -> T {
    try await withThrowingTaskGroup(of: T.self) { group in
        group.addTask { try await operation() }
        group.addTask { try await Task.sleep(for: .milliseconds(500)); throw URLError(.timedOut) }
        defer { group.cancelAll() }
        return try await group.next()!
    }
}
#endif
