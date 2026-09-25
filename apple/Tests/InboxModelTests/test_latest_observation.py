#!/usr/bin/env python3
"""Execute production observer against controlled history/stream suspension points.

Failure cases defined before implementation: cached/restarted cursors replay a stale
backlog; reconnects retain that stale cursor; reading anchors disappear during tail
refresh; skipped ranges masquerade as contiguous history; media/state block stream
startup; internal-only tails remain blank; late recovery mutates a different tab;
and failed snapshots advance cursors or erase the cached transcript.
"""
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[2]
model = (root / 'NanocodexInbox/InboxModel.swift').read_text()
def method(start, end):
    return model[model.index(start):model.index(end, model.index(start))]

source = r'''
import Foundation
typealias Cursor = Int
extension Int { var rawValue: String { String(self) } }
struct Payload {
    var string = ""
    subscript(_ key: String) -> Payload { self }
}
struct AgentEvent {
    let cursor: Cursor; var readable = true
    var type = "event", turnID = "turn", data = Payload()
}
struct TranscriptRow { var role: String }
struct InboxMediaProjection {}
actor TranscriptStreamProjection {
    func rows(_ events: [AgentEvent]) -> [TranscriptRow] { events.filter(\.readable).map { _ in TranscriptRow(role: "Agent") } }
}
struct ConversationHistory {
    var events: [AgentEvent]; var latest: Cursor; var hasMore: Bool
    var byteCounts: [Int]; var rows: [TranscriptRow]; var hasNewer = false
    var projector = TranscriptStreamProjection()
}
struct EventPage { var events: [AgentEvent]; var latest: Cursor; var hasMore: Bool }
struct SSEFrame { var event: AgentEvent?; var cursor: Cursor?; var payloadBytes = 1 }
enum APIError: Error, Equatable { case invalidResponse, agentDeleting, http(Int) }
enum TranscriptPreparation { static func byteCounts(_ events: [AgentEvent]) async throws -> [Int] { events.map { _ in 1 } } }
struct Card: Equatable {
    var id: String
    mutating func apply(state: Int) throws {}
    mutating func apply(events: [AgentEvent], transcriptRows: [TranscriptRow]) {}
}
struct Deck { var focusedID: String? = "agent" }
struct Preferences { func enqueue(_ body: (UserDefaults) -> Void) {} }
enum DemoContent { static func rows(_ id: String) -> [TranscriptRow] { [] } }
enum Signpost { case event }
let accountPerformanceLog = 0
func os_signpost(_ type: Signpost, log: Int, name: String, _ format: String, _ count: Int) {}
func snapshot(_ cursors: [Int], readable: Bool = true, more: Bool = true) -> ConversationHistory {
    .init(events: cursors.map { AgentEvent(cursor: $0, readable: readable) }, latest: cursors.last ?? 0,
          hasMore: more, byteCounts: cursors.map { _ in 1 }, rows: readable ? [TranscriptRow(role: "Agent")] : [])
}
@MainActor final class Client {
    var tails: [ConversationHistory] = []
    var pages: [EventPage] = []
    var tailReads = 0, olderReads = 0, olderFailures = 0
    var streamCursors: [Cursor] = []
    var failTail = false, holdTail = false, holdOlder = false
    var finishStream = false
    var duringOlder: ((Int) -> Void)?
    func prepare(_ id: String) async throws {}
    func state(_ id: String) async throws -> Int {
        while true { try await Task.sleep(for: .milliseconds(5)) }
    }
    func conversationHistory(_ id: String) async throws -> ConversationHistory {
        tailReads += 1
        while holdTail { try await Task.sleep(for: .milliseconds(5)) }
        if failTail { throw APIError.invalidResponse }
        guard !tails.isEmpty else { throw APIError.invalidResponse }
        return tails.removeFirst()
    }
    func history(_ id: String, before: Cursor) async throws -> EventPage {
        olderReads += 1
        duringOlder?(olderReads)
        if olderFailures > 0 { olderFailures -= 1; throw URLError(.networkConnectionLost) }
        while holdOlder { try await Task.sleep(for: .milliseconds(5)) }
        guard !pages.isEmpty else { throw APIError.invalidResponse }
        return pages.removeFirst()
    }
    func stream(_ id: String, after: Cursor, receive: @escaping (SSEFrame) async -> Void) async throws {
        streamCursors.append(after)
        await receive(SSEFrame(event: AgentEvent(cursor: after + 1, readable: false), cursor: after + 1))
        while !finishStream { try await Task.sleep(for: .milliseconds(5)) }
        finishStream = false
    }
}
@MainActor final class Model {
    struct TabHistory {
        var events: [AgentEvent]; var cursor: Cursor; var hasOlder: Bool
        var hasNewer = false; var newerAfter: Cursor?; var additionalGaps: [Cursor] = []; var bytes: [Int]; var rows: [TranscriptRow]
        var retainedBytes: Int; var projector: TranscriptStreamProjection; var media = InboxMediaProjection()
        var followingLatest = true; var protectedCursors: ClosedRange<Cursor>?
    }
    var deck = Deck(), observedAgentID: String? = "agent"
    var focused: Card? { cards.first { $0.id == deck.focusedID } }
    var cards = [Card(id: "agent"), Card(id: "other")]
    var client: Client? = Client()
    var isDemo = false, isActive = true, connected = true
    var focusedHistoryLoaded = true, streamReceivedFrame = false
    var events = [AgentEvent(cursor: 1)], eventBytes = [1], retainedBytes = 1, cursor = 1
    var rows = [TranscriptRow(role: "Agent")]
    var streamProjector = TranscriptStreamProjection(), mediaProjection = InboxMediaProjection()
    var tabHistories: [String: TabHistory] = [:], recentTabs: [String] = []
    var focusedState: Task<Void, Never>?, focusedHistoryRequest: Task<ConversationHistory, Error>?
    var openingHistory: (id: String, request: Task<ConversationHistory, Error>)?
    var readableHistoryRecovery: Task<Void, Never>?
    var streaming: Task<Void, Never>?, projection: Task<Void, Never>?
    var generation = UUID()
    var observation = UUID() { didSet { readableHistoryRecovery?.cancel(); readableHistoryRecovery = nil } }
    var projectedFirstCursor: Cursor?, olderBefore: Cursor? = 1, newerAfter: Cursor?
    var hasOlder = true, hasNewer = false, loadingOlder = false, loadingNewer = false, followingLatest = true
    var additionalHistoryGaps: [Cursor] = []
    var latestJumpEvents: [(event: AgentEvent, bytes: Int)]?
    var protectedHistoryCursors: ClosedRange<Cursor>?, protectedHistorySelection: Int?
    var historyMutationRevision = UUID()
    var threadError: String?, error: String?, threadLoading = false, connection = "Live", selectedTurn = ""
    var pendingCreations: Set<String> = [], demoRows: [String: [TranscriptRow]] = [:]
    var scope = "", preferences = Preferences(), historyCursors: [String: Cursor] = [:]
    var mediaScheduled = false
    var needsLatestHistory: Bool { hasNewer && events.last?.cursor == newerAfter }
    func trimTabCache() {}
    func cancelOverview(_ id: String) {}
    func resumeOverview() {}
    func cancelOlderHistoryPrefetch() {}
    func reconcilePending(id: String, events: [AgentEvent], state: Card? = nil) {}
    func publishPreparedRows(_ rows: [TranscriptRow], media: InboxMediaProjection) { self.rows = rows }
    func prepareMedia(_ rows: [TranscriptRow]) async -> InboxMediaProjection {
        while !Task.isCancelled { try? await Task.sleep(for: .milliseconds(5)) }
        return InboxMediaProjection()
    }
    func scheduleMediaPreparation() { mediaScheduled = true }
    func scheduleProjection(id: String, epoch: UUID, token: UUID, delay: Duration = .zero) {
        rows = events.filter(\.readable).map { _ in TranscriptRow(role: "Agent") }
    }
    func trimMeasuredEvents(towardOlder: Bool, keeping: Int = 1) {}
    func forgetUnavailableAgent(_ id: String) {}
    func ingestDelta(_ position: Int) {
        receive(SSEFrame(event: AgentEvent(cursor: position, data: Payload(string: "assistant.delta")), cursor: position), id: "agent", epoch: generation, token: observation)
    }
    func start() { observeFocused(restart: true) }
    func stop() { streaming?.cancel(); focusedState?.cancel(); focusedHistoryRequest?.cancel(); observation = UUID() }
'''
source += method('    private func observeFocused(', '    func overviewRows(')
source += method('    private func receive(_ frame:', '    private func scheduleProjection(')
source += method('    func setHistoryAtLatest(', '    func protectHistoryRows(')
source += r'''
}
@MainActor final class ProjectionModel {
    var generation = UUID(), observation = UUID()
    var projection: Task<Void, Never>?
    var calls: [CheckedContinuation<Bool, Never>] = []
    func projectEvents(id: String, epoch: UUID, token: UUID) async -> Bool {
        await withCheckedContinuation { calls.append($0) }
    }
    func start() { scheduleProjection(id: "agent", epoch: generation, token: observation, delay: .zero) }
'''
source += method('    private func scheduleProjection(', '    private func publishPreparedRows(')
source += r'''
}
@MainActor func eventually(_ condition: () -> Bool) async {
    for _ in 0..<600 {
        if condition() { return }
        try? await Task.sleep(for: .milliseconds(5))
    }
    preconditionFailure("timed out waiting for observer")
}
@main struct Tests {
    @MainActor static func main() async {
        let projecting = ProjectionModel()
        projecting.start()
        await eventually { projecting.calls.count == 1 }
        projecting.projection?.cancel(); projecting.projection = nil
        projecting.start()
        await eventually { projecting.calls.count == 2 }
        projecting.calls[0].resume(returning: false)
        try? await Task.sleep(for: .milliseconds(20))
        precondition(projecting.projection != nil)
        projecting.calls[1].resume(returning: false)
        await eventually { projecting.projection == nil }

        let tab = Model()
        tab.followingLatest = false; tab.protectedHistoryCursors = 1...1
        tab.client!.tails = [snapshot([50])]
        tab.deck.focusedID = "other"; tab.start()
        await eventually { tab.client!.streamCursors == [50] }
        tab.client!.tails = [snapshot([90, 100])]
        tab.deck.focusedID = "agent"; tab.start()
        await eventually { tab.client!.streamCursors == [50, 100] }
        precondition(tab.events.map(\.cursor) == [1, 90, 100, 101])
        precondition(!tab.followingLatest && tab.protectedHistoryCursors == 1...1 && tab.newerAfter == 1)
        tab.stop()

        let cached = Model()
        cached.client!.tails = [snapshot([90, 100]), snapshot([190, 200])]
        cached.client!.holdTail = true
        cached.start()
        await eventually { cached.client!.tailReads == 1 }
        precondition(cached.cursor == 1 && cached.client!.streamCursors.isEmpty && cached.events.map(\.cursor) == [1])
        cached.client!.holdTail = false
        await eventually { cached.client!.streamCursors == [100] }
        precondition(cached.events.map(\.cursor) == [90, 100, 101] && cached.mediaScheduled)
        cached.client!.finishStream = true
        await eventually { cached.client!.streamCursors == [100, 200] }
        precondition(cached.events.map(\.cursor) == [190, 200, 201])
        cached.stop()

        let reading = Model()
        reading.followingLatest = false; reading.protectedHistoryCursors = 1...1
        reading.client!.tails = [snapshot([90, 100]), snapshot([190, 200])]
        reading.start()
        await eventually { reading.client!.streamCursors == [100] }
        precondition(reading.events.map(\.cursor) == [1, 90, 100, 101])
        precondition(reading.protectedHistoryCursors == 1...1 && reading.hasNewer && reading.newerAfter == 1)
        reading.client!.finishStream = true
        await eventually { reading.client!.streamCursors == [100, 200] }
        precondition(reading.events.map(\.cursor) == [1, 90, 100, 101, 190, 200, 201])
        precondition(reading.newerAfter == 1 && reading.additionalHistoryGaps == [101])
        reading.stop()

        let overlap = Model()
        overlap.followingLatest = false
        overlap.client!.tails = [snapshot([1, 2])]
        overlap.start()
        await eventually { overlap.client!.streamCursors == [2] }
        precondition(overlap.events.map(\.cursor) == [1, 2, 3] && !overlap.hasNewer)
        overlap.stop()

        // Reconnecting inside a delta-only page must keep a cached turn prefix.
        let prefix = Model()
        prefix.events = [AgentEvent(cursor: 1, type: "turn_accepted"), AgentEvent(cursor: 2, data: Payload(string: "assistant.delta"))]
        prefix.eventBytes = [1, 1]; prefix.cursor = 2
        var deltaTail = snapshot([2, 3])
        deltaTail.events = [AgentEvent(cursor: 2, data: Payload(string: "assistant.delta")), AgentEvent(cursor: 3, data: Payload(string: "assistant.delta"))]
        prefix.client!.tails = [deltaTail]
        prefix.start()
        await eventually { prefix.client!.streamCursors == [3] }
        precondition(prefix.events.map(\.cursor) == [1, 2, 3, 4])
        prefix.stop()

        // A fresh partial delta stays live while its contiguous prefix is recovered.
        let partial = Model()
        partial.events = []; partial.eventBytes = []; partial.cursor = 0
        partial.client!.tails = [deltaTail]
        partial.client!.holdOlder = true
        partial.client!.pages = [.init(events: [AgentEvent(cursor: 1, type: "turn_accepted")], latest: 3, hasMore: false)]
        partial.start()
        await eventually { partial.client!.streamCursors == [3] && partial.client!.olderReads == 1 }
        partial.client!.holdOlder = false
        await eventually { partial.events.first?.cursor == 1 }
        precondition(partial.events.map(\.cursor) == [1, 2, 3, 4] && !partial.hasOlder)
        partial.stop()

        // Empty terminal text cannot repair the missing prefix of earlier deltas.
        let emptyFinal = Model()
        emptyFinal.events = []; emptyFinal.eventBytes = []; emptyFinal.cursor = 0
        var emptyTail = deltaTail
        emptyTail.events.append(AgentEvent(cursor: 4, readable: false, type: "turn_completed"))
        emptyTail.latest = 4; emptyTail.byteCounts.append(1)
        emptyFinal.client!.tails = [emptyTail]
        emptyFinal.client!.pages = [.init(events: [AgentEvent(cursor: 1, type: "turn_accepted")], latest: 4, hasMore: false)]
        emptyFinal.start()
        await eventually { emptyFinal.events.first?.cursor == 1 }
        precondition(emptyFinal.client!.olderReads == 1)
        emptyFinal.stop()

        // Recovery may exceed its byte target by one bounded page, but must not
        // evict the live tail to retain more and more prefix pages.
        let bounded = Model()
        bounded.client!.tails = [snapshot([90, 100], readable: false)]
        bounded.client!.holdOlder = true
        bounded.client!.pages = [.init(events: [AgentEvent(cursor: 70)], latest: 100, hasMore: true)]
        bounded.start()
        await eventually { bounded.client!.olderReads == 1 }
        bounded.retainedBytes = 16 * 1024 * 1024
        bounded.client!.holdOlder = false
        try? await Task.sleep(for: .milliseconds(30))
        precondition(bounded.events.map(\.cursor) == [90, 100, 101] && bounded.hasOlder)
        bounded.stop()

        // A live retention trim invalidates a pending backward boundary.
        let trimmed = Model()
        trimmed.client!.tails = [snapshot([90, 100], readable: false)]
        trimmed.client!.holdOlder = true
        trimmed.client!.pages = [.init(events: [AgentEvent(cursor: 70)], latest: 100, hasMore: true)]
        trimmed.start()
        await eventually { trimmed.client!.olderReads == 1 }
        trimmed.events.removeFirst(); trimmed.eventBytes.removeFirst(); trimmed.olderBefore = 100
        trimmed.client!.holdOlder = false
        try? await Task.sleep(for: .milliseconds(30))
        precondition(trimmed.events.map(\.cursor) == [100, 101] && !trimmed.hasNewer)
        trimmed.stop()

        // User scrolls up while recovery is suspended: preserve the row anchor.
        let scrolled = Model()
        scrolled.client!.tails = [deltaTail]
        scrolled.client!.holdOlder = true
        scrolled.client!.pages = [.init(events: [AgentEvent(cursor: 1, type: "turn_accepted")], latest: 3, hasMore: false)]
        scrolled.start()
        await eventually { scrolled.client!.olderReads == 1 }
        scrolled.followingLatest = false; scrolled.protectedHistoryCursors = 2...2
        scrolled.client!.holdOlder = false
        try? await Task.sleep(for: .milliseconds(30))
        precondition(scrolled.events.first?.cursor == 2 && scrolled.protectedHistoryCursors == 2...2)
        scrolled.client!.pages = [.init(events: [AgentEvent(cursor: 1, type: "turn_accepted")], latest: 3, hasMore: false)]
        scrolled.setHistoryAtLatest(true)
        await eventually { scrolled.events.first?.cursor == 1 }
        scrolled.stop()

        let failed = Model()
        failed.client!.failTail = true
        failed.start()
        await eventually { failed.connection == "Reconnecting" }
        precondition(failed.cursor == 1 && failed.events.map(\.cursor) == [1] && failed.client!.streamCursors.isEmpty)
        failed.stop()

        let recovery = Model()
        recovery.client!.tails = [snapshot([90, 100], readable: false)]
        recovery.client!.pages = [
            .init(events: [AgentEvent(cursor: 80, readable: false)], latest: 100, hasMore: true),
            .init(events: [AgentEvent(cursor: 70)], latest: 100, hasMore: true)]
        recovery.client!.holdOlder = true
        recovery.start()
        await eventually { recovery.client!.streamCursors == [100] && recovery.client!.olderReads == 1 }
        precondition(recovery.cursor == 101)
        recovery.client!.holdOlder = false
        await eventually { recovery.events.contains { $0.cursor == 70 } }
        precondition(recovery.events.map(\.cursor) == [70, 90, 100, 101])
        precondition(recovery.cursor == 101 && recovery.hasNewer && recovery.newerAfter == 70)
        recovery.stop()

        // A transient lookup failure must retry while the healthy stream stays up.
        let retry = Model()
        retry.client!.tails = [snapshot([90, 100], readable: false)]
        retry.client!.olderFailures = 1
        retry.client!.pages = [.init(events: [AgentEvent(cursor: 70)], latest: 100, hasMore: false)]
        retry.start()
        await eventually { retry.events.first?.cursor == 70 }
        precondition(retry.client!.streamCursors == [100] && retry.client!.olderReads == 2 && retry.cursor == 101)
        retry.stop()

        // Live delta arrives after scratch scanning discarded an internal page.
        // Restart at the retained tail so the answer prefix stays contiguous.
        let racing = Model()
        racing.client!.tails = [snapshot([90, 100], readable: false)]
        racing.client!.pages = [
            .init(events: [AgentEvent(cursor: 80, readable: false)], latest: 100, hasMore: true),
            .init(events: [AgentEvent(cursor: 70, type: "turn_accepted")], latest: 100, hasMore: false),
            .init(events: [AgentEvent(cursor: 80, readable: false)], latest: 102, hasMore: true),
            .init(events: [AgentEvent(cursor: 70, type: "turn_accepted")], latest: 102, hasMore: false)]
        racing.client!.duringOlder = { count in
            if count == 2 {
                racing.ingestDelta(102)
            }
        }
        racing.start()
        await eventually { racing.events.first?.cursor == 70 }
        precondition(racing.events.map(\.cursor) == [70, 80, 90, 100, 101, 102] && !racing.hasNewer)
        racing.stop()

        let skippedPrefix = Model()
        skippedPrefix.client!.tails = [snapshot([90, 100], readable: false)]
        skippedPrefix.client!.pages = [
            .init(events: [AgentEvent(cursor: 80, readable: false)], latest: 100, hasMore: true),
            .init(events: [AgentEvent(cursor: 70, data: Payload(string: "assistant.delta"))], latest: 100, hasMore: true),
            .init(events: [AgentEvent(cursor: 60, type: "turn_accepted")], latest: 100, hasMore: false)]
        skippedPrefix.start()
        await eventually { skippedPrefix.events.first?.cursor == 60 }
        precondition(skippedPrefix.newerAfter == 70 && skippedPrefix.events.map(\.cursor) == [60, 70, 90, 100, 101])
        skippedPrefix.stop()

        let switched = Model()
        switched.client!.tails = [snapshot([90, 100], readable: false)]
        switched.client!.holdOlder = true
        switched.client!.pages = [.init(events: [AgentEvent(cursor: 70)], latest: 100, hasMore: false)]
        switched.start()
        await eventually { switched.client!.olderReads == 1 }
        switched.stop(); switched.deck.focusedID = "other"
        switched.client!.holdOlder = false
        try? await Task.sleep(for: .milliseconds(30))
        precondition(!switched.events.contains { $0.cursor == 70 })
        print("PASS: cached/restart and reconnect tail handoff, read anchor and gap, overlap dedupe, failed snapshot, nonblocking readable recovery, cancellation")
    }
}
'''
with tempfile.TemporaryDirectory(prefix='inbox-observer-tests-') as directory:
    path = Path(directory)
    (path / 'ObservationTests.swift').write_text(source)
    subprocess.run(['swiftc', '-parse-as-library', str(path / 'ObservationTests.swift'), '-o', str(path / 'tests')], check=True)
    subprocess.run([str(path / 'tests')], check=True)
