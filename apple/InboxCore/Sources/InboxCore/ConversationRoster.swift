import Foundation

/// Stable navigation order: live summary updates must not move a row under a
/// finger. Explicitly construct a new roster when changing account or project.
public struct ConversationRoster: Equatable, Sendable {
    public private(set) var ids: [String]

    public init(cards: [AgentCard]) {
        var seen = Set<String>()
        ids = cards.sorted(by: AgentCard.mostRecentFirst).map(\.id).filter { seen.insert($0).inserted }
    }

    public mutating func reconcile(_ cards: [AgentCard]) {
        let available = Set(cards.map(\.id))
        ids.removeAll { !available.contains($0) }
        var known = Set(ids)
        ids.append(contentsOf: cards.map(\.id).filter { known.insert($0).inserted })
    }

    public func visible(in cards: [AgentCard], matching query: String) -> [AgentCard] {
        let byID = Dictionary(cards.map { ($0.id, $0) }, uniquingKeysWith: { _, last in last })
        let query = query.trimmingCharacters(in: .whitespacesAndNewlines)
        return ids.compactMap { byID[$0] }.filter { card in
            query.isEmpty || card.title.localizedCaseInsensitiveContains(query)
                || card.id.localizedCaseInsensitiveContains(query)
                || card.preview.localizedCaseInsensitiveContains(query)
        }
    }
}
