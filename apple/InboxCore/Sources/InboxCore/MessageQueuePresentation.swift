import Foundation

/// Show the head message immediately; only actual follow-ups waiting behind
/// another turn belong in the queue. Delivery bookkeeping remains independent.
public struct MessageQueuePresentation {
    public let messages: [PendingMessage]
    public let rows: [TranscriptRow]
    public let attachmentNames: [String: [String]]
    public var queuedMessages: [PendingMessage] { messages.filter { !$0.predecessor.isEmpty } }

    public init(agentID: String, rows: [TranscriptRow], pending: [PendingMessage], activeTurns: [String],
                cancelledTurns: Set<String> = [], executingTurns: Set<String> = []) {
        var byID = Dictionary(uniqueKeysWithValues: pending.filter { $0.agentID == agentID }.map { ($0.id, $0) })
        var userRows: [String: TranscriptRow] = [:]
        for row in rows where row.role == "You" && userRows[row.turnID ?? row.id] == nil { userRows[row.turnID ?? row.id] = row }
        for (id, existing) in byID where existing.remoteAdmission == true {
            guard let row = userRows[id] else { continue }
            var updated = PendingMessage(agentID: agentID, input: row.text, predecessor: existing.predecessor, id: id)
            updated.remoteAdmission = true
            updated.phase = existing.phase
            updated.error = existing.error
            updated.acceptedCursor = existing.acceptedCursor ?? row.cursor
            byID[id] = updated
        }
        let executed = executingTurns.union(rows.compactMap { row in
            ["Agent", "Thinking", "Tool"].contains(row.role) ? row.turnID : nil
        })
        for id in executed { byID.removeValue(forKey: id) }
        for (index, turnID) in activeTurns.enumerated() where byID[turnID] == nil && !cancelledTurns.contains(turnID)
            && !executed.contains(turnID) {
            let row = userRows[turnID]
            var message = PendingMessage(agentID: agentID, input: row?.text ?? "Message queued on another device", predecessor: index > 0 ? activeTurns[index - 1] : "", id: turnID)
            message.remoteAdmission = true
            message.phase = .queued
            message.acceptedCursor = row?.cursor
            byID[turnID] = message
        }
        // A stale predecessor is not a lifecycle state. The displayed queue
        // must advance even before an older local send receipt is reconciled.
        let ordered = activeTurns.enumerated().compactMap { index, id -> PendingMessage? in
            guard var message = byID.removeValue(forKey: id) else { return nil }
            if message.phase == .queued { message.predecessor = index > 0 ? activeTurns[index - 1] : "" }
            return message
        }
        messages = ordered + pending.compactMap { $0.agentID == agentID ? byID.removeValue(forKey: $0.id) : nil }
        attachmentNames = Dictionary(uniqueKeysWithValues: messages.map { message in
            let row = userRows[message.id]
            let names = (message.attachments ?? []).map(\.name) + (row?.imageFiles ?? []).map(\.name) + (row?.videos ?? []).map(\.name)
            var seen = Set<String>()
            return (message.id, names.filter { seen.insert($0).inserted })
        })
        let queuedIDs = Set(messages.filter { !$0.predecessor.isEmpty }.map(\.id))
        var displayed = rows.compactMap { row -> TranscriptRow? in
            let turnID = row.turnID ?? row.id
            guard !queuedIDs.contains(turnID) else { return nil }
            var row = row
            if row.role == "You", row.turnID != nil, !row.id.contains(":voice:") { row.id = turnID + ":user" }
            if row.role == "You", cancelledTurns.contains(turnID) {
                row.role = "Status"
                row.text = "Cancelled request: " + row.text
            }
            return row
        }
        for message in messages where message.predecessor.isEmpty && userRows[message.id] == nil && message.remoteAdmission != true {
            var row = TranscriptRow(id: message.id + ":user", role: "You", text: message.input)
            row.turnID = message.id
            displayed.append(row)
        }
        self.rows = displayed
    }
}
