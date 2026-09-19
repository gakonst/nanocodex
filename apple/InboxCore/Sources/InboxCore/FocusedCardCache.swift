import Foundation

/// Cache both a found card and a missing selection. Roster mutations invalidate
/// the snapshot; switching focus uses the ID key without comparing card arrays.
public struct FocusedCardCache {
    private var cached: (id: String?, card: AgentCard?)?
    private var location: (id: String, index: Int)?
    private(set) var lookupCount = 0
    public init() {}
    public mutating func invalidate() { cached = nil }
    public mutating func card(id: String?, in cards: [AgentCard]) -> AgentCard? {
        if let cached, cached.id == id { return cached.card }
        let index: Int?
        if let id, let location, location.id == id, cards.indices.contains(location.index), cards[location.index].id == id {
            index = location.index
        } else {
            index = id.flatMap { id in cards.firstIndex { $0.id == id } }
        }
        let card = index.map { cards[$0] }
        location = index.flatMap { index in id.map { ($0, index) } }
        lookupCount += 1
        cached = (id, card)
        return card
    }
}
