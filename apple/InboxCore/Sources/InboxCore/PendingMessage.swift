import Foundation

/// A durable follow-up is admitted once. "Steer now" only cancels its captured
/// predecessor; it never submits the follow-up a second time.
public struct PendingMessage: Identifiable, Codable, Equatable, Sendable {
    public enum Phase: String, Codable, Sendable { case submitting, queued, starting, cancelling, failed }
    public let id: String
    public let agentID: String
    public let input: String
    public let predecessor: String
    public var phase: Phase = .submitting
    public var acceptedCursor: Cursor?
    public var error: String?
    public init(agentID: String, input: String, predecessor: String, id: String = UUID().uuidString) {
        self.id = id; self.agentID = agentID; self.input = input; self.predecessor = predecessor
    }
    public var submission: AgentCommand { AgentCommand(agentID: agentID, input: input, kind: .followUp, requestID: id) }
    public var interruption: AgentCommand? {
        guard phase == .queued, !predecessor.isEmpty, predecessor != id else { return nil }
        return AgentCommand(agentID: agentID, turnID: predecessor, kind: .stop)
    }
    public func hasStarted(in events: [AgentEvent]) -> Bool {
        events.contains { event in
            guard event.turnID == id else { return false }
            if ["turn_completed", "turn_cancelled", "turn_failed"].contains(event.type) { return true }
            return event.type == "event" && ["run.started", "assistant.delta", "assistant.message", "reasoning.summary.delta", "tool.call", "tool.result"].contains(event.data["event"]["type"].string)
        }
    }
    public func hasFinished(activeTurns: [String], stateCursor: Cursor) -> Bool {
        guard let acceptedCursor, stateCursor >= acceptedCursor else { return false }
        return !activeTurns.contains(id)
    }
    public mutating func restore() {
        if phase == .submitting { phase = .failed; error = "Delivery unconfirmed. Retry uses the same message ID." }
        if phase == .starting || phase == .cancelling { phase = .queued }
    }
}
