import Foundation

/// Device-local organization of persistent managed conversations.
public struct InboxProject: Codable, Equatable, Identifiable, Sendable {
    public var id: String
    public var name: String
    public var primaryAgentID: String
    public var agentIDs: [String]
    public init(id: String, name: String, primaryAgentID: String, agentIDs: [String]? = nil) {
        self.id = id; self.name = name; self.primaryAgentID = primaryAgentID
        self.agentIDs = agentIDs ?? [primaryAgentID]
    }
    public mutating func replaceAgent(_ old: String, with new: String) {
        if primaryAgentID == old { primaryAgentID = new }
        var seen = Set<String>()
        agentIDs = agentIDs.map { $0 == old ? new : $0 }.filter { seen.insert($0).inserted }
    }
}

/// One roster snapshot shared by project navigation and message child links.
public struct InboxProjectIndex {
    public let projects: [InboxProject]
    public let cardsByID: [String: AgentCard]
    private let links: [String: [String: [AgentCard]]]

    public init(cards: [AgentCard], savedProjects: [InboxProject]) {
        let cardsByID = Dictionary(cards.map { ($0.id, $0) }, uniquingKeysWith: { _, last in last })
        self.cardsByID = cardsByID
        let available = Set(cardsByID.keys)
        var members: [String: [String]] = [:]
        var links: [String: [String: [AgentCard]]] = [:]
        for card in cards {
            if let root = card.projectRootID, root != card.id { members[root, default: []].append(card.id) }
            if let parent = card.parentAgentID, let turn = card.originTurnID {
                links[parent, default: [:]][turn, default: []].append(card)
            }
        }
        self.links = links
        let saved = savedProjects.filter { project in
            guard let card = cardsByID[project.primaryAgentID] else { return false }
            return card.projectRootID == nil || card.projectRootID == card.id
                || !available.contains(card.projectRootID!)
        }.map { project in
            InboxProject(id: project.id, name: cardsByID[project.primaryAgentID]?.projectName ?? project.name, primaryAgentID: project.primaryAgentID,
                         agentIDs: [project.primaryAgentID] + (members[project.primaryAgentID] ?? []))
        }
        let assigned = Set(saved.flatMap(\.agentIDs))
        projects = saved + cards.filter { !assigned.contains($0.id)
            && ($0.projectRootID == nil || !available.contains($0.projectRootID!) || $0.projectRootID == $0.id)
        }.sorted(by: AgentCard.mostRecentFirst).map {
            InboxProject(id: "project-" + $0.id, name: $0.projectName ?? $0.title, primaryAgentID: $0.id,
                         agentIDs: [$0.id] + (members[$0.id] ?? []))
        }
    }
    public func children(parentAgentID: String, originTurnID: String) -> [AgentCard] {
        links[parentAgentID]?[originTurnID] ?? []
    }
}

/// A task is a real admitted turn, never an inferred split of the user's input.
public struct ProjectTask: Identifiable, Equatable, Sendable {
    public var id: String { agentID + ":" + turnID }
    public var agentID: String
    public var turnID: String
    public var title: String
    public var status: String
    public var rows: [TranscriptRow]
    public init(agentID: String, turnID: String, title: String, status: String, rows: [TranscriptRow]) {
        self.agentID = agentID; self.turnID = turnID; self.title = title; self.status = status; self.rows = rows
    }
    public var isLive: Bool { status == "Working" || status == "Queued" || status == "Sending" }

    /// The project list needs one status per child, not every turn's message arrays.
    /// Match the active-or-most-recent selection from `project` without grouping history.
    public static func summary(agentID: String, rows: [TranscriptRow], events: [AgentEvent],
                               activeTurns: [String], pending: [PendingMessage], isRunning: Bool = false) -> Self? {
        // Active turns are admitted even when their input is outside loaded history.
        // The project UI already gives a running card's status precedence over history.
        if isRunning, let id = activeTurns.first, !id.isEmpty {
            return Self(agentID: agentID, turnID: id, title: "Task", status: "Working", rows: [])
        }
        let pending = pending.filter { $0.agentID == agentID && !$0.id.isEmpty }
        let id: String
        if let active = activeTurns.first, !active.isEmpty {
            id = active
        } else {
            var seen = Set<String>()
            var latest: String?
            for row in rows {
                if let id = row.turnID, !id.isEmpty, seen.insert(id).inserted { latest = id }
            }
            for id in activeTurns where !id.isEmpty {
                if seen.insert(id).inserted { latest = id }
            }
            for message in pending {
                if seen.insert(message.id).inserted { latest = message.id }
            }
            guard let latest else { return nil }
            id = latest
        }
        let status: String
        let outcome = events.last { $0.turnID == id && ["turn_completed", "turn_failed", "turn_cancelled"].contains($0.type) }
        if let outcome {
            status = outcome.type == "turn_completed" ? "Completed" : outcome.type == "turn_failed" ? "Failed" : "Stopped"
        } else if activeTurns.first == id { status = "Working" }
        else if activeTurns.contains(id) { status = "Queued" }
        else if let message = pending.first(where: { $0.id == id }) { status = message.phase == .failed ? "Failed" : "Sending" }
        else { status = rows.contains { $0.turnID == id && $0.role == "Agent" && $0.phase == "final" && !$0.running } ? "Completed" : "History" }
        return Self(agentID: agentID, turnID: id, title: "Task", status: status, rows: [])
    }

    public static func project(agentID: String, rows: [TranscriptRow], events: [AgentEvent], activeTurns: [String], pending: [PendingMessage]) -> [Self] {
        var order: [String] = []
        var grouped: [String: [TranscriptRow]] = [:]
        func admit(_ id: String) {
            guard !id.isEmpty, grouped[id] == nil else { return }
            order.append(id); grouped[id] = []
        }
        for row in rows {
            guard let id = row.turnID, !id.isEmpty else { continue }
            admit(id); grouped[id, default: []].append(row)
        }
        for id in activeTurns { admit(id) }
        let pending = pending.filter { $0.agentID == agentID }
        for message in pending { admit(message.id) }
        var outcomes: [String: String] = [:]
        for event in events {
            switch event.type {
            case "turn_completed": outcomes[event.turnID] = "Completed"
            case "turn_failed": outcomes[event.turnID] = "Failed"
            case "turn_cancelled": outcomes[event.turnID] = "Stopped"
            default: break
            }
        }
        return order.reversed().map { id in
            let content = grouped[id] ?? []
            let message = pending.first { $0.id == id }
            let input = content.first { $0.role == "You" }?.text ?? message?.input ?? ""
            let title = input
            let hasFinal = content.contains { $0.role == "Agent" && $0.phase == "final" && !$0.running }
            let status: String
            if let outcome = outcomes[id] { status = outcome }
            else if activeTurns.first == id { status = "Working" }
            else if activeTurns.contains(id) { status = "Queued" }
            else if let message { status = message.phase == .failed ? "Failed" : "Sending" }
            else { status = hasFinal ? "Completed" : "History" }
            return Self(agentID: agentID, turnID: id,
                        title: title.isEmpty ? "Task" : String(title.prefix(180)), status: status, rows: content)
        }
    }
}

/// Lightweight summaries live for the roster lifetime, independently of the bounded
/// detail-history cache. History producers invalidate only the child they changed.
public struct ProjectTaskSummaryCache {
    private struct Entry {
        var activeTurns: [String]
        var pending: [PendingMessage]
        var summary: ProjectTask
    }
    private var entries: [String: Entry] = [:]
    public init() {}
    public mutating func invalidate(agentID: String) { entries[agentID] = nil }
    public mutating func removeAll() { entries.removeAll() }
    public mutating func value(agentID: String, activeTurns: [String], pending: [PendingMessage],
                               compute: () -> ProjectTask) -> ProjectTask {
        if let entry = entries[agentID], entry.activeTurns == activeTurns, entry.pending == pending {
            return entry.summary
        }
        let summary = compute()
        entries[agentID] = Entry(activeTurns: activeTurns, pending: pending, summary: summary)
        return summary
    }
}
