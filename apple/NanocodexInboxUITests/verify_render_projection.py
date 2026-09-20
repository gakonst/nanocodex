#!/usr/bin/env python3
"""Run the production render scheduler with slow synthetic preparation (macOS)."""
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
view = (root / 'NanocodexInbox/InboxView.swift').read_text()
projection = view[view.index('@MainActor\nprivate final class ConversationRenderProjection'):
                  view.index('\nprivate struct ConversationView:')]
fixtures = r'''
import Foundation
import Combine
import os
struct TranscriptRow: Sendable { var id: String }
struct PendingMessage: Sendable {}
struct ConversationRenderedItem: Sendable {
    var id: String
    static func project(_ rows: [TranscriptRow], outputs: [String: String]) -> [Self] {
        rows.map { Self(id: $0.id) }
    }
}
struct ConversationItem {
    static func group(_ rows: [TranscriptRow], activeTurns: [String]) -> [TranscriptRow] {
        Thread.sleep(forTimeInterval: 0.03)
        return rows
    }
}
struct Queue: Sendable { var rows: [TranscriptRow]; var messages: [PendingMessage] = [] }
struct Card { var activeTurns: [String] = [] }
@MainActor final class InboxModel {
    var focusedTranscriptRevision = UUID()
    var focusedConversationIdentity: String? = "thread"
    var focused: Card? = Card()
    var generatedOutputsByRow: [String: String] = [:]
    var index = 0
    func prepareFocusedQueue() async -> Queue? {
        let queue = Queue(rows: [TranscriptRow(id: String(index))])
        try? await Task.sleep(for: .milliseconds(30))
        return queue
    }
}
'''
checks = r'''
@main struct Test {
    @MainActor static func main() async {
        let model = InboxModel()
        let projection = ConversationRenderProjection()
        var progress = Set<String>()
        for index in 1...100 {
            model.index = index
            model.focusedTranscriptRevision = UUID()
            projection.request(model, identity: "thread")
            try? await Task.sleep(for: .milliseconds(5))
            if let id = projection.value?.rows.first?.id { progress.insert(id) }
        }
        precondition(progress.count >= 3, "Continuous revisions must publish intermediate snapshots")
        for _ in 0..<100 {
            if projection.value?.rows.first?.id == "100" { break }
            try? await Task.sleep(for: .milliseconds(10))
        }
        precondition(projection.value?.rows.first?.id == "100", "Must catch up to final revision")
        let settledCount = projection.rebuildCount
        projection.request(model, identity: "thread")
        try? await Task.sleep(for: .milliseconds(100))
        precondition(projection.rebuildCount == settledCount, "Unchanged input must reuse the projection")
        let previous = projection.value?.revision
        model.focusedTranscriptRevision = UUID()
        projection.request(model, identity: "thread")
        try? await Task.sleep(for: .milliseconds(5))
        model.focusedConversationIdentity = "other"
        try? await Task.sleep(for: .milliseconds(150))
        precondition(projection.value?.revision == previous, "Changed focus must fence publication")
        model.focusedConversationIdentity = "thread"
        projection.request(model, identity: "thread")
        try? await Task.sleep(for: .milliseconds(5))
        projection.cancel()
        model.index = 101
        model.focusedTranscriptRevision = UUID()
        projection.request(model, identity: "thread")
        for _ in 0..<100 {
            if projection.value?.rows.first?.id == "101" { break }
            try? await Task.sleep(for: .milliseconds(10))
        }
        precondition(projection.value?.rows.first?.id == "101", "Cancel/reappear must restart")
        print("render projection: progress, catch-up, reuse, focus fencing, restart passed")
    }
}
'''
with tempfile.TemporaryDirectory() as directory:
    source = Path(directory) / 'test.swift'
    binary = Path(directory) / 'test'
    source.write_text(fixtures + projection + checks)
    subprocess.run(['swiftc', '-parse-as-library', str(source), '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
