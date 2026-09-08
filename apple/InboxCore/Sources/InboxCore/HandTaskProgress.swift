import Foundation

public enum HandTaskOutcome: String, Sendable {
    case completed = "Completed", failed = "Failed", stopped = "Stopped", paused = "Paused"
}

/// Count actual activity for one durable turn. Replayed events, other turns,
/// and heartbeats must never manufacture progress to extend background time.
public struct HandTaskProgress: Sendable {
    public let turnID: String
    public private(set) var cursor = Cursor.zero
    public private(set) var completedUnits: Int64 = 0
    public private(set) var detail = "Waiting for agent"
    public private(set) var outcome: HandTaskOutcome?
    public init(turnID: String, after cursor: Cursor = .zero) { self.turnID = turnID; self.cursor = cursor }

    /// Final receipt/local runtime outcome must update the system presentation
    /// even when the terminal stream event was missed. This invents no progress.
    public mutating func finish(_ value: HandTaskOutcome) {
        guard outcome == nil else { return }
        outcome = value; detail = value.rawValue
    }

    @discardableResult
    public mutating func receive(_ event: AgentEvent) -> Bool {
        guard event.cursor > cursor else { return false }
        cursor = event.cursor
        guard event.turnID == turnID, outcome == nil else { return false }
        switch event.type {
        case "turn_completed": finish(.completed)
        case "turn_failed": finish(.failed)
        case "turn_cancelled": finish(.stopped)
        case "event":
            switch event.data["event"]["type"].string {
            case "run.started": detail = "Agent working"
            case "tool.call": detail = "Using tools"
            case "tool.result": detail = "Tool finished"
            case "assistant.delta", "assistant.message": detail = "Writing response"
            case "reasoning.summary.delta": detail = "Thinking"
            default: return false
            }
        default: return false
        }
        completedUnits += 1
        return true
    }
}
