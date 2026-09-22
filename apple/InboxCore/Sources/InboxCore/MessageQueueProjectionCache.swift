import Foundation

/// A focused reading window's derived queue. The owner must invalidate when
/// rows, pending messages, steering, active turns, focus, or demo mode mutate;
/// event replacement (including prepend, trim, and reset) invalidates history.
/// Reads deliberately do not compare arrays or retain a second raw history.
public struct MessageQueueProjectionCache: Sendable {
    public private(set) var revision = UUID()
    private var cached: (agentID: String, value: MessageQueuePresentation)?
    private var history: (cancelled: Set<String>, started: Set<String>)?
    // Internal counters let tests verify actual work, not just equal output.
    private(set) var projectionCount = 0
    private(set) var historyScanCount = 0

    public init() {}

    public mutating func invalidate() {
        revision = UUID()
        cached = nil
    }

    public mutating func invalidateHistory() {
        history = nil
        invalidate()
    }

    public mutating func presentation(agentID: String, events: [AgentEvent], rows: [TranscriptRow],
                                      pending: [PendingMessage], steeringTransfers: [SteeringTransfer],
                                      activeTurns: [String], isDemo: Bool) -> MessageQueuePresentation {
        if let cached, cached.agentID == agentID { return cached.value }
        if history == nil {
            var cancelled = Set<String>(), started = Set<String>()
            for event in events {
                if event.type == "turn_cancelled" { cancelled.insert(event.turnID) }
                if event.type == "event" {
                    switch event.data["event"]["type"].string {
                    case "run.started", "assistant.delta", "assistant.message", "reasoning.summary.delta", "tool.call", "tool.result":
                        started.insert(event.turnID)
                    default: break
                    }
                }
            }
            history = (cancelled, started)
            historyScanCount += 1
        }
        let transfers = steeringTransfers.filter { $0.agentID == agentID }
        var transfersByTurn: [String: SteeringTransfer] = [:]
        for transfer in transfers where transfersByTurn[transfer.sourceTurnID] == nil {
            transfersByTurn[transfer.sourceTurnID] = transfer
        }
        let transferred = Set(transfersByTurn.keys)
        let pendingIDs = Set(pending.map(\.id))
        var displayed = transfers.isEmpty ? rows : rows.compactMap { row -> TranscriptRow? in
            guard let transfer = transfersByTurn[row.turnID ?? row.id] else { return row }
            guard transfer.wasAccepted || transfer.phase == .withdrawn
                || (transfer.phase == .unconfirmed && !pendingIDs.contains(transfer.id)) else { return nil }
            guard row.role == "You" else { return nil }
            var row = row
            if transfer.phase == .withdrawn { row.role = "Status"; row.text = "Steering withdrawn: " + row.text }
            else if !transfer.wasAccepted { row.role = "Status"; row.text = "Steering delivery unconfirmed: " + row.text; row.detail = transfer.error ?? "" }
            else { row.detail = transfer.error ?? (transfer.phase == .withdrawing ? transfer.title : "") }
            return row
        }
        // Direct steering has no separate turn_accepted user row. Retain the
        // original message after the pending delivery record is retired.
        let visibleTurns = Set(rows.compactMap(\.turnID)).union(activeTurns)
        let directTransfers = transfers.filter { $0.direct == true }
        var windowFirst: Cursor?, windowLast: Cursor?
        if !directTransfers.isEmpty {
            windowFirst = events.first?.cursor; windowLast = events.last?.cursor
            if events.isEmpty {
                for row in rows {
                    guard let cursor = row.cursor else { continue }
                    windowFirst = min(windowFirst ?? cursor, cursor)
                    windowLast = max(windowLast ?? cursor, cursor)
                }
            }
        }
        // Stable sort preserves tap order for multiple corrections at one cursor.
        let chronological = directTransfers.enumerated().sorted { lhs, rhs in
            let left = lhs.element.sourceCursor ?? .zero, right = rhs.element.sourceCursor ?? .zero
            return left == right ? lhs.offset < rhs.offset : left < right
        }.map(\.element)
        for transfer in chronological where (transfer.wasAccepted || transfer.phase == .withdrawn || transfer.phase == .unconfirmed || transfer.error != nil)
            && (visibleTurns.contains(transfer.targetTurnID) || pendingIDs.contains(transfer.id)) {
            guard let input = transfer.sourceInput,
                  !displayed.contains(where: { $0.turnID == transfer.id || $0.id == transfer.id || $0.id == transfer.id + ":user" }) else { continue }
            if !pendingIDs.contains(transfer.id), let anchor = transfer.sourceCursor {
                if let first = windowFirst, anchor < first { continue }
                if let last = windowLast, anchor > last { continue }
            }
            let media = TranscriptInput(transfer.sourcePayload ?? .string(input))
            var row = TranscriptRow(id: transfer.id + ":user", role: "You", text: media.text,
                                    images: media.images.isEmpty ? nil : media.images)
            row.imageFiles = media.imageFiles.isEmpty ? nil : media.imageFiles
            row.videos = media.videos.isEmpty ? nil : media.videos
            row.turnID = transfer.id
            row.detail = transfer.error ?? (transfer.phase == .withdrawing ? transfer.title : "")
            if transfer.phase == .withdrawn {
                row.role = "Status"; row.text = "Steering withdrawn: " + row.text
            } else if !transfer.wasAccepted, !pendingIDs.contains(transfer.id) {
                row.role = "Status"; row.text = "Steering delivery unconfirmed: " + row.text
            }
            row.cursor = transfer.sourceCursor
            if let anchor = transfer.sourceCursor {
                let insertion = displayed.firstIndex { ($0.cursor.map { $0 > anchor }) == true } ?? displayed.endIndex
                displayed.insert(row, at: insertion)
            } else if let anchorID = transfer.sourceRowID, let index = displayed.firstIndex(where: { $0.id == anchorID }) {
                // Legacy row-only anchors remain behind previous local corrections.
                var insertion = index + 1
                while insertion < displayed.count, chronological.contains(where: { $0.id == displayed[insertion].turnID && $0.sourceRowID == anchorID }) { insertion += 1 }
                displayed.insert(row, at: insertion)
            } else {
                displayed.append(row)
            }
        }
        let active = activeTurns.filter { !transferred.contains($0) }
        let value = MessageQueuePresentation(agentID: agentID, rows: displayed, pending: pending,
            activeTurns: active, cancelledTurns: (history?.cancelled ?? []).subtracting(transferred),
            executingTurns: (isDemo ? Set(active.prefix(1)) : history?.started ?? []).subtracting(transferred))
        projectionCount += 1
        cached = (agentID, value)
        return value
    }
}
