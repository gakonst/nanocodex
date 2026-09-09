import Foundation
import CryptoKit

/// One native notification per conversation. Reuse the bounded, user-facing
/// projection without putting other agents or private outbox input in the body.
public struct AgentThreadNotification: Equatable, Sendable, Identifiable {
    public let id: String
    public let revision: String
    public let title: String
    public let subtitle: String
    public let body: String
    public let isRunning: Bool

    public var fingerprint: String {
        let data = (try? JSONEncoder().encode([revision, title, subtitle, body])) ?? Data()
        return SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
    }

    public static func make(cards: [AgentCard], seen: [String: Cursor], deferred: [String: Cursor],
                            pending: [PendingMessage]) -> [Self] {
        let messages = Dictionary(grouping: pending, by: \.agentID)
        return cards.sorted(by: AgentCard.mostRecentFirst).compactMap { card in
            let outbox = messages[card.id] ?? []
            let snapshot = AgentActivitySnapshot.make(cards: [card], seen: seen, deferred: deferred,
                                                      paused: true, pending: outbox)
            guard let entry = snapshot.entries.first, !entry.id.isEmpty else { return nil }
            let phase: String
            switch entry.status {
            case "delivery": phase = "Delivery unconfirmed"
            case "failed": phase = "Turn failed"
            case "ready": phase = "Response ready"
            default: phase = "Running when last checked"
            }
            let turn = card.activeTurns.first ?? "outcome-" + card.outcomeCursor.rawValue
            let delivery = outbox.filter { $0.phase == .failed }.map(\.id).sorted().joined(separator: ",")
            let revision = entry.status + ":" + turn + ":" + delivery
            var body = entry.detail
            if let queued = entry.queued, queued > 0 {
                body += "\n\(queued) queued \(queued == 1 ? "follow-up" : "follow-ups")."
            }
            if card.isRunning { body += "\nOpen for current status." }
            return Self(id: card.id, revision: revision, title: entry.title,
                        subtitle: phase, body: body, isRunning: card.isRunning)
        }
    }
}

/// Persist only IDs and hashes. Clearing a thread suppresses its current phase
/// across polling and relaunch; a new turn or terminal outcome can notify again.
public struct AgentNotificationLedger: Codable, Sendable {
    public struct Receipt: Codable, Sendable {
        public var revision: String
        public var fingerprint: String
    }
    public private(set) var tracked = Set<String>()
    public private(set) var published: [String: Receipt] = [:]
    public private(set) var dismissed: [String: String] = [:]
    public init() {}

    public mutating func reconcile(_ threads: [AgentThreadNotification], retaining unchecked: Set<String>) -> Set<String> {
        let allowed = Set(threads.map(\.id)).union(unchecked)
        let removed = tracked.subtracting(allowed)
        tracked.formIntersection(allowed)
        published = published.filter { allowed.contains($0.key) }
        dismissed = dismissed.filter { allowed.contains($0.key) }
        tracked.formUnion(threads.filter(\.isRunning).map(\.id))
        return removed
    }

    public func shouldPublish(_ thread: AgentThreadNotification, foreground: Bool) -> Bool {
        !foreground && tracked.contains(thread.id) && dismissed[thread.id] != thread.revision
            && published[thread.id]?.fingerprint != thread.fingerprint
    }

    public mutating func didPublish(_ thread: AgentThreadNotification) {
        published[thread.id] = Receipt(revision: thread.revision, fingerprint: thread.fingerprint)
    }

    public mutating func dismiss(id: String, revision: String) {
        // Late callbacks for older notifications must not suppress newer work.
        guard published[id]?.revision == revision else { return }
        dismissed[id] = revision
    }
}
