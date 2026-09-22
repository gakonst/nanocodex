import Foundation

/// Chronological feed entries retain the identity and position of their source
/// row. Tool completion updates its card in place; turns never reorder the feed.
public struct ConversationItem: Identifiable, Equatable, Sendable {
    public let id: String
    public var message: TranscriptRow?
    /// Code Mode batches retain the parent first, followed by its individual calls.
    /// Keeping all rows here also preserves generated outputs and history anchors.
    public var activity: [TranscriptRow] = []
    public var childAgentID: String?
    public var isRunning = false
    public var isCodeModeBatch: Bool { activity.first?.tool?.title == "Run code" }

    public static func group(_ rows: [TranscriptRow], activeTurns: [String] = []) -> [ConversationItem] {
        // Keep canonical rows and their anchors, but expose child content only
        // through a labeled activity disclosure, never as a parent answer.
        var childGroups: [String: [TranscriptRow]] = [:]
        for row in rows where row.agentID != nil {
            let key = (row.turnID ?? "") + "\0" + row.agentID!
            childGroups[key, default: []].append(row)
        }
        var emittedChildren = Set<String>()
        let batches = Set(rows.filter { $0.role == "Tool" && $0.tool?.title == "Run code" }.map(\.id))
        var children: [String: [TranscriptRow]] = [:]
        var nested = Set<String>()
        for row in rows where row.role == "Tool" {
            // The runtime constructs nested IDs as "<parent call ID>/code-<number>".
            // Transcript IDs already scope that relationship by turn and agent.
            // Never infer parentage from proximity: concurrent calls can interleave.
            guard let marker = row.id.range(of: "/code-", options: .backwards),
                  !row.id[marker.upperBound...].isEmpty,
                  row.id[marker.upperBound...].allSatisfy({ $0.isASCII && $0.isNumber }) else { continue }
            var parent = String(row.id[..<marker.lowerBound])
            guard batches.contains(parent) else { continue }
            // Flatten deeper nested batches into their visible root; every row
            // and generated output must remain reachable.
            while let ancestorMarker = parent.range(of: "/code-", options: .backwards) {
                let suffix = parent[ancestorMarker.upperBound...]
                let ancestor = String(parent[..<ancestorMarker.lowerBound])
                guard !suffix.isEmpty, suffix.allSatisfy({ $0.isASCII && $0.isNumber }),
                      batches.contains(ancestor) else { break }
                parent = ancestor
            }
            children[parent, default: []].append(row)
            nested.insert(row.id)
        }
        func live(_ row: TranscriptRow) -> Bool {
            row.running && row.turnID != nil && row.turnID == activeTurns.first
        }
        return rows.compactMap { row in
            if let agent = row.agentID {
                let key = (row.turnID ?? "") + "\0" + agent
                guard emittedChildren.insert(key).inserted else { return nil }
                let activity = childGroups[key] ?? [row]
                return .init(id: row.id, activity: activity, childAgentID: agent, isRunning: activity.contains(where: live))
            }
            guard !nested.contains(row.id) else { return nil }
            if row.role == "Tool" {
                let activity = [row] + (children[row.id] ?? [])
                return .init(id: row.id, activity: activity, isRunning: activity.contains(where: live))
            }
            return .init(id: row.id, message: row, isRunning: live(row))
        }
    }
}
