#!/usr/bin/env python3
"""Run actual InboxModel history methods with a deterministic transport, without an app build."""
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
struct AgentEvent { let cursor: Cursor }
struct SSEFrame { var event: AgentEvent?; var cursor: Cursor?; var payloadBytes = 1 }
struct ConversationHistory { var events: [AgentEvent]; var byteCounts: [Int]; var latest: Cursor; var hasMore: Bool; var hasNewer = false }
struct EventPage { var events: [AgentEvent]; var latest: Cursor; var hasMore: Bool }
enum APIError: Error { case invalidResponse }
enum TranscriptPreparation {
    static func byteCounts(_ events: [AgentEvent]) async throws -> [Int] { events.map { _ in 1 } }
}
@MainActor final class Client {
    var pages: [EventPage] = []
    var requests: [Cursor?] = []
    var duringRequest: (() -> Void)?
    func conversationHistory(_ id: String) async throws -> ConversationHistory {
        .init(events: [AgentEvent(cursor: 8)], byteCounts: [1], latest: 8, hasMore: true)
    }
    func history(_ id: String, after: Cursor?) async throws -> EventPage {
        requests.append(after)
        duringRequest?(); duringRequest = nil
        guard !pages.isEmpty else { throw APIError.invalidResponse }
        return pages.removeFirst()
    }
}
@MainActor final class Model {
    struct Focused { let id = "agent" }
    var focused: Focused? = Focused()
    var client: Client? = Client()
    var generation = UUID(), observation = UUID()
    var hasNewer = false, hasOlder = false, loadingNewer = false, loadingOlder = false
    var followingLatest = false
    var historyMutationRevision = UUID()
    var newerAfter: Cursor?
    var newerCatchUp: Task<Void, Never>?
    var latestJumpEvents: [(event: AgentEvent, bytes: Int)]?
    var events: [AgentEvent] = [AgentEvent(cursor: 1)]
    var eventBytes = [1], retainedBytes = 1, cursor = 1
    var protectedHistoryCursors: ClosedRange<Cursor>?
    var protectedHistorySelection: Int?
    var olderBefore: Cursor?, projectedFirstCursor: Cursor?
    var projection: Task<Void, Never>?
    var connection = "Live", streamReceivedFrame = false, threadLoading = false
    var threadError: String?
    var projected: [Int] = []
    func reconcilePending(id: String, events: [AgentEvent]) {}
    func trimMeasuredEvents(towardOlder: Bool) {}
    func cancelOlderHistoryPrefetch() {}
    func scheduleProjection(id: String, epoch: UUID, token: UUID, delay: Duration = .zero) {
        projected = events.map(\.cursor)
    }
    func ingest(_ cursor: Int) {
        receive(SSEFrame(event: AgentEvent(cursor: cursor), cursor: cursor), id: "agent", epoch: generation, token: observation)
    }
'''
source += method('    private func receive(_ frame:', '    private func scheduleProjection(')
source += method('    func loadNewer(', '    private func cancelOlderHistoryPrefetch()')
source += r'''
}
@main struct Tests {
    @MainActor static func main() async {
        let reading = Model()
        reading.protectedHistoryCursors = 1...1
        reading.ingest(2)
        precondition(reading.projected == [1, 2] && !reading.hasNewer)
        precondition(reading.protectedHistoryCursors == 1...1)
        reading.loadingOlder = true; reading.ingest(3)
        reading.loadingOlder = false; reading.loadingNewer = true; reading.ingest(4)
        precondition(reading.projected == [1, 2, 3, 4])

        let gap = Model()
        gap.hasNewer = true; gap.newerAfter = 1; gap.cursor = 3
        gap.ingest(4)
        precondition(gap.projected == [1, 4])
        gap.client!.pages = [.init(events: [2, 3, 4].map(AgentEvent.init), latest: 4, hasMore: false)]
        await gap.loadNewer()
        precondition(gap.client!.requests == [1])
        precondition(gap.projected == [1, 2, 3, 4] && !gap.hasNewer && gap.newerAfter == nil)
        precondition(gap.eventBytes.count == gap.events.count)

        let empty = Model()
        empty.hasNewer = true; empty.newerAfter = 1; empty.cursor = 9
        empty.client!.pages = [.init(events: [], latest: 9, hasMore: false)]
        await empty.loadNewer()
        precondition(!empty.hasNewer && empty.newerAfter == nil)

        let jump = Model()
        jump.client!.pages = [.init(events: [8, 9].map(AgentEvent.init), latest: 9, hasMore: true)]
        jump.client!.duringRequest = { jump.ingest(10) }
        await jump.loadNewer(latest: true)
        precondition(jump.client!.requests.count == 1 && jump.client!.requests[0] == nil)
        precondition(jump.projected == [8, 9, 10] && !jump.hasNewer && jump.followingLatest)
        jump.ingest(11)
        precondition(jump.projected == [8, 9, 10, 11])

        let paging = Model()
        paging.hasNewer = true; paging.newerAfter = 1; paging.cursor = 5
        paging.ingest(6)
        paging.client!.pages = [
            .init(events: [2, 3].map(AgentEvent.init), latest: 6, hasMore: true),
            .init(events: [4, 5, 6].map(AgentEvent.init), latest: 6, hasMore: false)
        ]
        await paging.loadNewer()
        await paging.loadNewer()
        precondition(paging.client!.requests == [1, 3])
        precondition(paging.projected == [1, 2, 3, 4, 5, 6] && !paging.hasNewer)
        print("PASS: immediate live admission, read protection, in-flight paging, gap merge, empty terminal page, latest snapshot race, bounded multi-page catch-up")
    }
}
'''
with tempfile.TemporaryDirectory(prefix='inbox-model-tests-') as directory:
    path = Path(directory)
    (path / 'HistoryTests.swift').write_text(source)
    subprocess.run(['swiftc', '-parse-as-library', str(path / 'HistoryTests.swift'), '-o', str(path / 'tests')], check=True)
    subprocess.run([str(path / 'tests')], check=True)
