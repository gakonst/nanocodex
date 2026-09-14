import Foundation

/// A read-only projection of the managed agent's existing cron trigger contract.
public struct ScheduledJob: Identifiable, Equatable, Sendable {
    public var id: String { agentID + "/" + triggerID }
    public let agentID: String
    public let triggerID: String
    public let cron: String
    public let timezone: String
    public let input: String
    public let enabled: Bool
    public let startsNewConversation: Bool
    public let nextRun: Date?
    public let lastRun: Date?
    public let lastSkipped: Date?
    public let lastRunAgentID: String?

    public init(_ value: JSON, agentID: String) throws {
        _ = try ManagedClient.agentPath(agentID)
        let triggerID = value["id"].string
        guard !triggerID.isEmpty, triggerID.count <= 64,
              triggerID.utf8.allSatisfy({ (48...57).contains($0) || (65...90).contains($0) || (97...122).contains($0) || [45, 95].contains($0) }),
              case .string(let cron) = value["cron"], case .string(let timezone) = value["timezone"],
              case .string(let input) = value["input"], case .bool(let enabled) = value["enabled"],
              [.null, .string("new"), .string("continue")].contains(value["session_mode"]) else { throw APIError.invalidResponse }
        self.agentID = agentID; self.triggerID = triggerID
        self.cron = cron; self.timezone = timezone; self.input = input; self.enabled = enabled
        startsNewConversation = value["session_mode"] == .string("new")
        nextRun = try Self.date(value["next_run_at"])
        lastRun = try Self.date(value["last_run_at"])
        lastSkipped = try Self.date(value["last_skipped_at"])
        if value["last_agent_id"] != .null {
            let id = value["last_agent_id"].string
            _ = try ManagedClient.agentPath(id)
            lastRunAgentID = id
        } else {
            // Older continue-mode responses did not include last_agent_id.
            lastRunAgentID = !startsNewConversation && !value["last_turn_id"].string.isEmpty ? agentID : nil
        }
    }

    private static func date(_ value: JSON) throws -> Date? {
        if value == .null { return nil }
        guard case .number(let milliseconds) = value, milliseconds.isFinite, milliseconds >= 0,
              milliseconds <= 9_007_199_254_740_991, milliseconds.rounded(.down) == milliseconds else { throw APIError.invalidResponse }
        return Date(timeIntervalSince1970: milliseconds / 1000)
    }
}
