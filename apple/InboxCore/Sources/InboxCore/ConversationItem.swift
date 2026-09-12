import Foundation

/// One disclosure per durable turn, independent of interleaved tool calls or
/// subagents. Unknown/legacy assistant text stays visible; only explicit
/// commentary is folded away. Final answers and errors remain conversation rows.
public struct ConversationItem: Identifiable, Equatable {
    public let id: String
    public var message: TranscriptRow?
    public var activity: [TranscriptRow] = []
    public var isRunning = false

    public static func group(_ rows: [TranscriptRow], activeTurns: [String] = []) -> [ConversationItem] {
        // The service returns unfinished turns in admission order, including
        // queued follow-ups. Only the head can be executing. Keeping that order
        // also handles messages queued by another device or restored from history.
        let executingTurn = activeTurns.first
        var result: [ConversationItem] = [], indices: [String: Int] = [:]
        var lastIsActivity: [String: Bool] = [:], fallback = "history"
        var order: [String] = [], turns: [String: [TranscriptRow]] = [:]
        for row in rows {
            if row.role == "You" { fallback = row.id }
            let turn = row.turnID ?? (row.id.contains(":") ? String(row.id.prefix(while: { $0 != ":" })) : fallback)
            if turns[turn] == nil { order.append(turn) }
            turns[turn, default: []].append(row)
        }
        for (turn, row) in order.flatMap({ turn in (turns[turn] ?? []).map { (turn, $0) } }) {
            let work = row.role == "Thinking" || row.role == "Tool"
                || (row.role == "Agent" && (row.phase == "commentary" || row.agentID != nil))
            lastIsActivity[turn] = work || row.role == "You"
            if work {
                if let index = indices[turn] { result[index].activity.append(row) }
                else {
                    indices[turn] = result.count
                    result.append(.init(id: "activity-" + turn, activity: [row]))
                }
            } else {
                result.append(.init(id: row.id, message: row))
                if row.role == "You", executingTurn == turn, indices[turn] == nil {
                    indices[turn] = result.count
                    result.append(.init(id: "activity-" + turn))
                }
            }
        }
        for (turn, index) in indices {
            result[index].isRunning = executingTurn == turn && lastIsActivity[turn] == true
        }
        return result.filter { $0.message != nil || !$0.activity.isEmpty || $0.isRunning }
    }
}
