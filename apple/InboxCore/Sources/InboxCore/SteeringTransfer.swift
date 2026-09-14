import Foundation

/// Moves an already-admitted follow-up into the active turn without cancelling
/// that active turn. The source queue fence and steer admission are separate.
public struct SteeringTransfer: Identifiable, Codable, Equatable, Sendable {
    public enum Phase: String, Codable, Sendable {
        case preparing, removingQueued, ready, sending, accepted, withdrawing, withdrawn, unconfirmed
    }
    public var id: String { sourceTurnID }
    public let agentID: String
    public let sourceTurnID: String
    public let targetTurnID: String
    public var phase: Phase = .preparing
    public var error: String?
    public var withdrawRequested = false
    public var wasAccepted = false
    public init(agentID: String, sourceTurnID: String, targetTurnID: String) {
        self.agentID = agentID; self.sourceTurnID = sourceTurnID; self.targetTurnID = targetTurnID
    }
    public var sourceCancellation: AgentCommand { .init(agentID: agentID, turnID: sourceTurnID, kind: .stop) }
    public func command(input: JSON) -> AgentCommand {
        var command = AgentCommand(agentID: agentID, turnID: targetTurnID, kind: .steer, requestID: id)
        command.rawInput = input
        return command
    }
    public var withdrawal: AgentCommand { .init(agentID: agentID, turnID: targetTurnID, kind: .withdrawSteer, requestID: id) }
    public func sourceIsFenced(_ receipt: JSON) -> Bool {
        receipt["turn_id"].string == sourceTurnID && ["cancelling", "cancelled"].contains(receipt["state"].string)
    }
    public func isAccepted(_ receipt: JSON) -> Bool {
        receipt["turn_id"].string == targetTurnID && receipt["state"].string == "steering"
    }
    public func withdrawalResult(_ receipt: JSON) -> Bool? {
        guard receipt["turn_id"].string == targetTurnID, receipt["message_id"].string == id,
              case .bool(let withdrawn) = receipt["withdrawn"] else { return nil }
        return withdrawn
    }
    public var title: String {
        switch phase {
        case .preparing, .removingQueued, .ready: return "Preparing steering…"
        case .sending: return "Sending steering…"
        case .accepted: return "Steering sent"
        case .withdrawing: return "Withdrawing steering…"
        case .withdrawn: return "Steering withdrawn"
        case .unconfirmed: return "Steering delivery unconfirmed"
        }
    }
    public var canResume: Bool { [.preparing, .removingQueued, .ready, .withdrawing].contains(phase) }
    public mutating func restore() {
        if phase == .sending {
            phase = .unconfirmed
            error = "The steering response was not received. It may already be in use; it will not be sent again automatically."
        }
    }
}
