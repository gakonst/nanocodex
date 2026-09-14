import Foundation
#if canImport(ActivityKit) && os(iOS)
import ActivityKit

public struct AgentActivityAttributes: ActivityAttributes {
    public typealias ContentState = AgentActivitySnapshot
    public var account: String
    public init(account: String) { self.account = account }
}
#endif

/// A bounded lock-screen projection. Counts cover the entire roster; rows
/// pair the most urgent outcome with ongoing work.
public struct AgentActivitySnapshot: Codable, Hashable, Sendable {
    public struct Entry: Codable, Hashable, Identifiable, Sendable {
        public var id: String
        public var title: String
        public var detail: String
        public var status: String
        public var action: String?
        public var queued: Int?
    }
    public var running: Int
    public var ready: Int
    public var failed: Int
    public var entries: [Entry]
    public var observedAt: Date
    public var paused: Bool
    public var deliveryFailures: Int?
    public var queued: Int?
    public var attentionCount: Int?
    public var conversationCount: Int?
    public var total: Int { conversationCount ?? running + ready + failed }
    public var needsAttention: Int { attentionCount ?? failed }
    public var headline: String {
        if needsAttention > 0 { return needsAttention == 1 ? "1 needs attention" : "\(needsAttention) need attention" }
        if ready > 0 { return "\(ready) ready to review" }
        return running > 0 ? "\(running) running" : "Finished"
    }

    public static func make(cards: [AgentCard], seen: [String: Cursor], deferred: [String: Cursor],
                            paused: Bool, now: Date = Date(), pending: [PendingMessage] = []) -> Self {
        let failedDelivery = Set(pending.filter { $0.phase == .failed }.map(\.agentID))
        let queues = Dictionary(grouping: pending.filter { $0.phase == .queued }, by: \.agentID).mapValues(\.count)
        func unread(_ card: AgentCard) -> Bool {
            card.needsAttention(seen: seen[card.id]) && !(deferred[card.id].map { card.latestCursor <= $0 } ?? false)
        }
        let eligible = cards.filter { card in
            failedDelivery.contains(card.id) || card.isRunning || unread(card)
        }
        func rank(_ card: AgentCard) -> Int { failedDelivery.contains(card.id) ? 0 : card.isRunning ? 3 : card.status == "Failed" ? 1 : 2 }
        let sorted = eligible.sorted {
            if rank($0) != rank($1) { return rank($0) < rank($1) }
            return AgentCard.mostRecentFirst($0, $1)
        }
        // Reserve one row for ongoing work so an unread backlog cannot hide
        // everything that is still running. Never rotate rows under a tap.
        var visible = Array(sorted.prefix(2))
        if let running = sorted.first(where: { $0.isRunning && !failedDelivery.contains($0.id) }),
           !visible.contains(where: { $0.isRunning && !failedDelivery.contains($0.id) }), !visible.isEmpty {
            visible = [visible[0], running]
        }
        let readyCount = eligible.filter { $0.status == "Ready" && unread($0) }.count
        let failedCards = eligible.filter { $0.status == "Failed" && unread($0) }
        let deliveryIDs = failedDelivery.intersection(Set(eligible.map(\.id)))
        return Self(
            running: eligible.filter(\.isRunning).count,
            ready: readyCount,
            failed: failedCards.count,
            entries: visible.map { card in
                let deliveryFailed = failedDelivery.contains(card.id)
                let detail: String
                if deliveryFailed { detail = "Message delivery unconfirmed. Open to retry." }
                else if card.error != nil { detail = "Status unavailable · open to reconnect" }
                else if card.isRunning { detail = card.activityDetail.isEmpty ? card.activitySummary : card.activityDetail }
                else if !card.outcomeSummary.isEmpty { detail = card.outcomeSummary }
                else if card.status == "Ready", !card.preview.isEmpty { detail = AgentActivityText.excerpt(card.preview) }
                else { detail = card.status == "Failed" ? "Turn failed. Open the conversation for details." : "New response ready to review." }
                let status = deliveryFailed ? "delivery" : card.isRunning ? "running" : card.status == "Failed" ? "failed" : "ready"
                return Entry(id: card.id.utf8.count <= 256 ? card.id : "",
                      title: bounded(card.title.isEmpty ? "Untitled conversation" : card.title, bytes: 160),
                      detail: bounded(detail, bytes: 240), status: status,
                      action: bounded(deliveryFailed ? "Retry delivery" : card.isRunning ? card.activitySummary : card.status == "Failed" ? "Follow up" : "Review response", bytes: 64),
                      queued: queues[card.id])
            },
            observedAt: eligible.compactMap(\.observedAt).min() ?? now,
            paused: paused || eligible.contains { $0.error != nil || $0.observedAt == nil },
            deliveryFailures: deliveryIDs.count, queued: eligible.reduce(0) { $0 + (queues[$1.id] ?? 0) },
            attentionCount: deliveryIDs.union(failedCards.map(\.id)).count, conversationCount: eligible.count)
    }

    private static func bounded(_ value: String, bytes: Int) -> String {
        var result = "", count = 0
        for character in value.split(whereSeparator: \.isWhitespace).joined(separator: " ") {
            let size = String(character).utf8.count
            guard count + size <= bytes else { break }
            result.append(character); count += size
        }
        return result
    }
}

/// Short plain-text excerpts from user-facing output only. Never apply this to
/// tool arguments/results or reasoning; those belong inside the conversation.
public enum AgentActivityText {
    public static func excerpt(_ text: String) -> String {
        let prose = String(text.prefix(2048)).components(separatedBy: "```").enumerated()
            .filter { $0.offset.isMultiple(of: 2) }.map(\.element).joined(separator: " ")
        let plain = String(prose.prefix(2048))
            .replacingOccurrences(of: #"!?\[([^\]]*)\]\([^)]*\)"#, with: "$1", options: .regularExpression)
            .replacingOccurrences(of: #"(?m)^\s*[#>\-*]+\s*"#, with: "", options: .regularExpression)
            .replacingOccurrences(of: "**", with: "").replacingOccurrences(of: "`", with: "")
        return String(plain.split(whereSeparator: \.isWhitespace).joined(separator: " ").prefix(240))
    }
}

public enum AgentActivityLink {
    public static func url(account: String, agentID: String?) -> URL {
        var parts = URLComponents()
        parts.scheme = "nanocodex"; parts.host = "activity"
        parts.queryItems = [URLQueryItem(name: "account", value: account)]
        if let agentID, !agentID.isEmpty { parts.queryItems?.append(.init(name: "agent", value: agentID)) }
        return parts.url!
    }

    public static func destination(_ url: URL, account: String) -> String? {
        guard let parts = URLComponents(url: url, resolvingAgainstBaseURL: false),
              parts.scheme == "nanocodex", parts.host == "activity", parts.path.isEmpty,
              parts.user == nil, parts.password == nil, parts.port == nil, parts.fragment == nil,
              parts.queryItems?.filter({ $0.name == "account" }).count == 1,
              parts.queryItems?.first(where: { $0.name == "account" })?.value == account,
              parts.queryItems?.filter({ $0.name == "agent" }).count == 1,
              let id = parts.queryItems?.first(where: { $0.name == "agent" })?.value,
              !id.isEmpty, id.utf8.count <= 256 else { return nil }
        return id
    }
}
