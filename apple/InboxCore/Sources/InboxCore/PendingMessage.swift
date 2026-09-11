import Foundation

/// A durable follow-up is admitted once. "Steer now" resolves and captures an
/// unfinished predecessor; it never submits the follow-up a second time.
public struct PendingMessage: Identifiable, Codable, Equatable, Sendable {
    public enum Phase: String, Codable, Sendable { case submitting, queued, starting, cancelling, failed }
    public let id: String
    public let agentID: String
    public let input: String
    public var predecessor: String
    public var phase: Phase = .submitting
    public var acceptedCursor: Cursor?
    public var error: String?
    public var attachments: [MessageAttachment]?
    public var contextIDs: [String]?
    public init(agentID: String, input: String, predecessor: String, id: String = UUID().uuidString, contextIDs: [String]? = nil, attachments: [MessageAttachment]? = nil) {
        self.id = id; self.agentID = agentID; self.input = input; self.predecessor = predecessor
        self.contextIDs = contextIDs; self.attachments = attachments
    }
    /// Admission stores a message durably; it does not mean execution began.
    /// Resolve from local delivery state and the server queue for other devices.
    public static func deliveryLabel(turnID: String, pending: PendingMessage?, activeTurns: [String]) -> String? {
        if let pending {
            switch pending.phase {
            case .submitting: return "Sending…"
            case .queued: return "Queued · not started"
            case .starting: return "Queued · stopping current turn…"
            case .cancelling: return "Cancelling…"
            case .failed: return "Delivery unconfirmed"
            }
        }
        if let index = activeTurns.firstIndex(of: turnID), index > 0 { return "Queued · not started" }
        return nil
    }
    public var submission: AgentCommand { AgentCommand(agentID: agentID, input: input, kind: .followUp, requestID: id) }
    public var interruption: AgentCommand? {
        guard phase == .queued, !predecessor.isEmpty, predecessor != id else { return nil }
        return AgentCommand(agentID: agentID, turnID: predecessor, kind: .stop)
    }
    /// The server lists unfinished turns in queue order. A predecessor captured
    /// when sending may since have completed or been cancelled on another device.
    public func interruption(activeTurns: [String]) -> AgentCommand? {
        guard phase == .queued else { return nil }
        guard activeTurns.contains(id) || (!predecessor.isEmpty && activeTurns.contains(predecessor)) else { return nil }
        let preceding = activeTurns.prefix { $0 != id }
        guard let target = preceding.first, target != id else { return nil }
        return AgentCommand(agentID: agentID, turnID: target, kind: .stop)
    }
    public mutating func acknowledge(_ receipt: JSON) throws {
        guard receipt["turn_id"].string == id else { throw APIError.invalidResponse }
        // A late send response must not undo a Stop/Steer tap made in flight.
        if phase == .submitting || phase == .failed { phase = .queued; error = nil }
        acceptedCursor = Cursor(rawValue: receipt["cursor"].string.isEmpty ? receipt["accepted_cursor"].string : receipt["cursor"].string)
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
    }
}

/// Cancellation is an idempotent intent, independent of submission and polling.
/// Persist it until the exact target is terminal (or a confirmed pre-admission
/// cancellation has fenced a turn that does not exist yet).
public struct PendingTurnCancellation: Identifiable, Codable, Equatable, Sendable {
    public let agentID: String
    public let turnID: String
    public var acknowledged = false
    public var error: String?
    public var id: String { agentID + ":" + turnID }
    public init(agentID: String, turnID: String) { self.agentID = agentID; self.turnID = turnID }
    public var command: AgentCommand { AgentCommand(agentID: agentID, turnID: turnID, kind: .stop) }
}
