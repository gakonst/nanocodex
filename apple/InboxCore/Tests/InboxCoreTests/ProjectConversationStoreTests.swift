#if canImport(Combine)
import XCTest
@testable import InboxCore

@MainActor final class ProjectConversationStoreTests: XCTestCase {
    @MainActor final class Transport: ProjectConversationTransport {
        let scope = ProjectConversationScope(projectID: "project", conversations: [.init(id: "a", title: "A"), .init(id: "b", title: "B")])
        var active: [String] = []
        var stateCursor = "0"
        func state(_ id: String) async throws -> JSON {
            .object(["agent_id": .string(id), "latest_event_cursor": .string(stateCursor), "active_turns": .array(active.map(JSON.string))])
        }
        var pages: [EventPage] = []
        var commands: [AgentCommand] = []
        var failSend = false
        var streamHandler: ((@escaping @Sendable (ProjectConversationFrame) async -> Void) async throws -> Void)?
        var streamFailures: [Error] = []
        var sendHandler: ((AgentCommand) async throws -> Void)?
        var onStream: (() -> Void)?
        var onStreamCancellation: (() -> Void)?
        var historyCalls: [(String, Cursor?, Cursor?)] = []
        var streams: [(Cursor, @Sendable (ProjectConversationFrame) async -> Void)] = []
        var historyHandler: ((String) async throws -> EventPage)?
        func history(_ id: String, before: Cursor?, after: Cursor?) async throws -> EventPage {
            historyCalls.append((id, before, after))
            if let historyHandler { return try await historyHandler(id) }
            return pages.removeFirst()
        }
        func stream(_ id: String, after: Cursor, receive: @escaping @Sendable (ProjectConversationFrame) async -> Void) async throws {
            streams.append((after, receive)); onStream?()
            if let streamHandler { try await streamHandler(receive) }
            if !streamFailures.isEmpty { throw streamFailures.removeFirst() }
            do { try await Task.sleep(nanoseconds: 60_000_000_000) }
            catch { if Task.isCancelled { onStreamCancellation?() }; throw error }
        }
        func send(_ command: AgentCommand) async throws {
            commands.append(command)
            if let sendHandler { try await sendHandler(command) }
            if failSend { throw URLError(.networkConnectionLost) }
        }
    }
    func event(_ n: Int, _ text: String) -> JSON {
        .object(["cursor": .string(String(n)), "type": .string("turn_accepted"), "turn_id": .string("t\(n)"), "input": .string(text)])
    }
    func page(_ events: [JSON], latest: Int, more: Bool = false) throws -> EventPage {
        try EventPage(.object(["data": .array(events), "latest_cursor": .string(String(latest)), "has_more": .bool(more)]))
    }
    func waitForStream(_ transport: Transport, count: Int = 1) async {
        if transport.streams.count < count {
            let started = expectation(description: "stream started")
            transport.onStream = { if transport.streams.count == count { started.fulfill() } }
            await fulfillment(of: [started], timeout: 2)
            transport.onStream = nil
        }
        XCTAssertEqual(transport.streams.count, count)
    }

    func testReconnectUsesAppliedCheckpointAndMalformedFailureIsTerminal() async throws {
        let transport = Transport(); transport.stateCursor = "99"
        transport.pages = [try page([], latest: 0)]
        let event = try AgentEvent(event(2, "two"))
        transport.streamHandler = { receive in
            if transport.streams.count == 1 {
                await receive(.init(event: event, cursor: event.cursor))
                await receive(.init(cursor: Cursor(rawValue: "3")))
                throw URLError(.networkConnectionLost)
            }
            throw APIError.invalidResponse
        }
        let store = ProjectConversationStore(transport: transport)
        await store.select("a"); await waitForStream(transport, count: 2)
        XCTAssertEqual(transport.streams.map { $0.0.rawValue }, ["0", "3"])
        try await Task.sleep(nanoseconds: 600_000_000)
        XCTAssertEqual(transport.streams.count, 2)
        XCTAssertEqual(store.rows.map(\.text), ["two"])
        XCTAssertEqual(store.connection, .disconnected)
        store.suspend()
    }

    func testReadReconnectAndFatalAuthorization() async throws {
        let transport = Transport(); transport.pages = [try page([event(1, "one")], latest: 1)]
        transport.stateCursor = "90"
        transport.streamFailures = [URLError(.networkConnectionLost), APIError.http(403)]
        let store = ProjectConversationStore(transport: transport)
        await store.select("a"); await waitForStream(transport, count: 2)
        XCTAssertEqual(transport.streams.map { $0.0.rawValue }, ["1", "1"])
        try await Task.sleep(nanoseconds: 600_000_000)
        XCTAssertEqual(transport.streams.count, 2)
        XCTAssertEqual(store.connection, .disconnected); XCTAssertNotNil(store.error)
        store.suspend()
    }

    func testStopCapturesSelectionAcrossSuspendedWrite() async throws {
        let transport = Transport(); transport.active = ["running", "queued"]
        transport.pages = [try page([], latest: 0), try page([], latest: 0)]
        let store = ProjectConversationStore(transport: transport)
        await store.select("a")
        let started = expectation(description: "stop started")
        var continuation: CheckedContinuation<Void, Never>?
        transport.sendHandler = { _ in await withCheckedContinuation { continuation = $0; started.fulfill() } }
        let stop = Task { await store.stop() }
        await fulfillment(of: [started], timeout: 2)
        await store.select("b")
        continuation?.resume(); await stop.value
        XCTAssertEqual(transport.commands.count, 1)
        XCTAssertEqual(transport.commands[0].agentID, "a")
        XCTAssertEqual(transport.commands[0].turnID, "running")
        store.suspend()
    }

    func testStateAheadOfPartialHistoryPreservesQueueAndStopTarget() async throws {
        let transport = Transport(); transport.active = ["running", "queued"]; transport.stateCursor = "99"
        transport.pages = [try page([event(1, "old")], latest: 88)]
        let store = ProjectConversationStore(transport: transport)
        await store.select("a"); await waitForStream(transport)
        XCTAssertEqual(store.activeTurns, ["running", "queued"])
        XCTAssertEqual(transport.streams[0].0.rawValue, "1")
        await store.stop()
        XCTAssertEqual(transport.commands.first?.agentID, "a")
        XCTAssertEqual(transport.commands.first?.turnID, "running")
        XCTAssertEqual(transport.commands.first?.kind, .stop)
        XCTAssertEqual(store.cards.first?.stateCursor.rawValue, "99")
        store.suspend()
    }

    func testScopeDraftAndExplicitStableRetry() async throws {
        let transport = Transport(); transport.pages = [try page([], latest: 0)]
        let store = ProjectConversationStore(transport: transport)
        await store.select("outside"); store.setDraft("bad", for: "outside")
        XCTAssertNil(store.selection); XCTAssertTrue(transport.historyCalls.isEmpty); XCTAssertTrue(store.drafts.isEmpty)
        await store.select("a"); store.setDraft("hello", for: "a")
        transport.failSend = true
        await store.send()
        XCTAssertEqual(transport.commands.count, 1)
        XCTAssertEqual(store.pending["a"]?.isSending, false)
        store.setDraft("next", for: "a")
        await store.send()
        XCTAssertEqual(transport.commands.count, 1)
        transport.failSend = false
        await store.retryPending(for: "a")
        XCTAssertEqual(transport.commands.count, 2)
        XCTAssertEqual(transport.commands[0], transport.commands[1])
        XCTAssertNotNil(store.pending["a"]); XCTAssertEqual(store.drafts["a"], "next")
        await waitForStream(transport)
        let admitted = try AgentEvent(.object(["cursor": .string("1"), "type": .string("turn_accepted"), "turn_id": .string(transport.commands[0].requestID), "input": .string("hello")]))
        await transport.streams[0].1(.init(event: admitted, cursor: admitted.cursor))
        XCTAssertNil(store.pending["a"])
        store.suspend()
    }

    func testReplayAndSuspensionFence() async throws {
        let transport = Transport(); transport.pages = [try page([event(1, "one")], latest: 1)]
        let store = ProjectConversationStore(transport: transport)
        await store.select("a"); await waitForStream(transport)
        XCTAssertEqual(transport.streams[0].0.rawValue, "1")
        let duplicate = try AgentEvent(event(1, "duplicate"))
        await transport.streams[0].1(ProjectConversationFrame(event: duplicate, cursor: duplicate.cursor))
        let next = try AgentEvent(event(2, "two"))
        await transport.streams[0].1(ProjectConversationFrame(event: next, cursor: next.cursor))
        try await Task.sleep(nanoseconds: 60_000_000)
        XCTAssertEqual(store.rows.map(\.text), ["one", "two"])
        let cancelled = expectation(description: "stream cancelled")
        transport.onStreamCancellation = { cancelled.fulfill() }
        store.suspend()
        await fulfillment(of: [cancelled], timeout: 2)
        let late = try AgentEvent(event(3, "late"))
        await transport.streams[0].1(ProjectConversationFrame(event: late, cursor: late.cursor))
        XCTAssertEqual(store.rows.map(\.text), ["one", "two"])
        XCTAssertEqual(store.connection, .suspended)
    }

    func testStaleHistoryCannotReplaceSelection() async throws {
        let transport = Transport()
        let firstPage = try page([event(1, "A")], latest: 1)
        let secondPage = try page([event(2, "B")], latest: 2)
        var continuation: CheckedContinuation<EventPage, Never>?
        let started = expectation(description: "history started")
        transport.historyHandler = { id in
            if id == "a" { return await withCheckedContinuation { continuation = $0; started.fulfill() } }
            return secondPage
        }
        let store = ProjectConversationStore(transport: transport)
        let first = Task { await store.select("a") }
        await fulfillment(of: [started], timeout: 2)
        await store.select("b")
        continuation?.resume(returning: firstPage)
        await first.value
        XCTAssertEqual(store.selection, "b"); XCTAssertEqual(store.rows.map(\.text), ["B"])
        XCTAssertFalse(store.isLoading); store.suspend()
    }

    func testOlderPagingPausesStreamAndResumeRestoresLatest() async throws {
        let transport = Transport()
        transport.pages = [try page([event(3, "three")], latest: 3, more: true), try page([event(1, "one"), event(2, "two")], latest: 3), try page([event(4, "four")], latest: 4, more: true)]
        let store = ProjectConversationStore(transport: transport, byteLimit: 1)
        await store.select("a"); await waitForStream(transport)
        await store.loadOlder()
        XCTAssertEqual(transport.historyCalls[1].1?.rawValue, "3")
        XCTAssertEqual(store.rows.map(\.text), ["one"])
        XCTAssertEqual(store.connection, .suspended)
        let late = try AgentEvent(event(5, "late"))
        await transport.streams[0].1(ProjectConversationFrame(event: late, cursor: late.cursor))
        XCTAssertEqual(store.rows.map(\.text), ["one"])
        await store.resume(); await waitForStream(transport, count: 2)
        XCTAssertEqual(store.rows.map(\.text), ["four"])
        XCTAssertEqual(transport.streams[1].0.rawValue, "4")
        store.suspend()
    }
    func testManagedAdapterRejectsOutsideScopeBeforeNetwork() async throws {
        let credential = try AccountCredential(origin: "https://example.invalid", apiKey: "ncx_live_" + String(repeating: "a", count: 12) + "_" + String(repeating: "b", count: 43))
        let client = ManagedClient(credential: credential)
        defer { client.close() }
        let transport = try ProjectConversationManagedTransport(client: client, authorizedScope: .init(projectID: "p", conversations: [.init(id: "a", title: "A")]))
        do { _ = try await transport.history("outside", before: nil, after: nil); XCTFail("accepted outside roster") }
        catch { XCTAssertEqual(error as? APIError, .http(403)) }
        do { try await transport.send(.init(agentID: "outside", input: "hello", kind: .followUp)); XCTFail("accepted outside roster") }
        catch { XCTAssertEqual(error as? APIError, .http(403)) }
        do { try await transport.stream("outside", after: .zero) { _ in }; XCTFail("accepted outside roster") }
        catch { XCTAssertEqual(error as? APIError, .http(403)) }
        do { try await transport.send(.init(agentID: "a", kind: .steer)); XCTFail("accepted unsupported command") }
        catch { XCTAssertEqual(error as? APIError, .invalidResponse) }
    }

    func testHistoryFailuresLeaveLoadingFlagsUsableAndOlderRowsIntact() async throws {
        let transport = Transport()
        transport.historyHandler = { _ in throw URLError(.notConnectedToInternet) }
        let store = ProjectConversationStore(transport: transport)
        await store.select("a")
        XCTAssertFalse(store.isLoading); XCTAssertNotNil(store.error)
        XCTAssertEqual(store.connection, .disconnected)
        transport.historyHandler = nil
        transport.pages = [try page([event(3, "three")], latest: 3, more: true)]
        await store.resume()
        transport.historyHandler = { _ in throw URLError(.notConnectedToInternet) }
        await store.loadOlder()
        XCTAssertFalse(store.isLoadingOlder); XCTAssertTrue(store.hasOlder)
        XCTAssertEqual(store.rows.map(\.text), ["three"])
        XCTAssertNotNil(store.error)
        store.suspend()
    }

    func testCancelledHistoryCannotLeaveLoadingStateStuck() async throws {
        let transport = Transport()
        let result = try page([event(1, "one")], latest: 1)
        let started = expectation(description: "history started")
        var continuation: CheckedContinuation<EventPage, Never>?
        transport.historyHandler = { _ in await withCheckedContinuation { continuation = $0; started.fulfill() } }
        let store = ProjectConversationStore(transport: transport)
        let task = Task { await store.select("a") }
        await fulfillment(of: [started], timeout: 2)
        task.cancel(); continuation?.resume(returning: result)
        await task.value
        XCTAssertFalse(store.isLoading); XCTAssertTrue(store.rows.isEmpty)
        XCTAssertEqual(store.connection, .suspended)
        XCTAssertTrue(transport.streams.isEmpty)
    }

}
#endif
