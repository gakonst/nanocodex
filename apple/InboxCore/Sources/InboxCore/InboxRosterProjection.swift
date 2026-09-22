import Foundation

/// Immutable inputs allow roster filtering and ranking on a background executor.
public struct InboxRosterProjection: Sendable {
    public enum Filter: String, Sendable { case inbox = "Inbox", running = "Running", all = "All" }
    public var cards: [AgentCard]
    public var focusedID: String?
    public var opened: Set<String>
    public var closed: Set<String>
    public var tabOrder: [String]
    public var pinnedID: String?
    public var filter: Filter
    public var seen: [String: String]
    public var deferred: [String: Cursor]

    public init(cards: [AgentCard], focusedID: String?, opened: Set<String>, closed: Set<String>,
                tabOrder: [String], pinnedID: String?, filter: Filter, seen: [String: String], deferred: [String: Cursor]) {
        self.cards = cards; self.focusedID = focusedID; self.opened = opened; self.closed = closed
        self.tabOrder = tabOrder; self.pinnedID = pinnedID; self.filter = filter; self.seen = seen; self.deferred = deferred
    }
    public struct Result: Sendable {
        public let opened: Set<String>
        public let tabs: [String]
        public let eligible: [String]
    }
    public func resolve() -> Result {
        let opened = opened.intersection(Set(cards.map(\.id)))
        let available = Set(cards.filter { !closed.contains($0.id) && ConversationWindow.includes($0,
            focusedID: focusedID, openedIDs: opened) }.map(\.id))
        var known = Set<String>()
        var tabs = tabOrder.filter { available.contains($0) && known.insert($0).inserted }
        tabs += cards.filter { available.contains($0.id) && !known.contains($0.id) }
            .sorted(by: AgentCard.mostRecentFirst).map(\.id)
        let eligible = cards.filter { card in
            guard !closed.contains(card.id), available.contains(card.id) else { return false }
            if card.id == pinnedID { return true }
            switch filter {
            case .inbox: return card.isInInbox(seen: seen[card.id].flatMap(Cursor.init(rawValue:)), deferred: deferred[card.id])
            case .running: return card.isRunning
            case .all: return true
            }
        }.map { (card: $0, attention: $0.needsAttention(seen: seen[$0.id].flatMap(Cursor.init(rawValue:)))) }
        .sorted { a, b in
            if a.attention != b.attention { return a.attention }
            return a.card.updatedAt != b.card.updatedAt ? a.card.updatedAt > b.card.updatedAt : a.card.id < b.card.id
        }.map { $0.card.id }
        return Result(opened: opened, tabs: tabs, eligible: eligible)
    }
}
