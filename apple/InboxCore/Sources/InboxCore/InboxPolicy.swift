import Foundation

public struct AgentCard: Identifiable, Sendable {
    public let id: String
    public var title: String
    public var updatedAt: Double
    public var turnCount: Int
    public var activeTurns: [String] = []
    public var stateCursor: Cursor = .zero
    public var latestCursor: Cursor = .zero
    public var status = "Checking"
    public var model = ""
    private var previewCursor: Cursor = .zero
    public var preview = "Loading the latest update…"
    public var checked = false
    public var error: String?
    public init(id: String, title: String, updatedAt: Double = 0, turnCount: Int = 0) {
        self.id = id; self.title = title; self.updatedAt = updatedAt; self.turnCount = turnCount
    }
    public var isRunning: Bool { !activeTurns.isEmpty }
    public func needsAttention(seen: Cursor?) -> Bool {
        checked && !isRunning && latestCursor > (seen ?? .zero) && (status == "Ready" || status == "Failed")
    }
    public mutating func apply(state: JSON) throws {
        guard let cursor = Cursor(rawValue: state["latest_event_cursor"].string), state["agent_id"].string == id,
              case .array = state["active_turns"] else { throw APIError.invalidResponse }
        guard cursor >= stateCursor else { return }
        activeTurns = state["active_turns"].array.map(\.string)
        stateCursor = cursor; latestCursor = max(latestCursor, cursor)
        model = state["settings"]["model"].string
        checked = true; error = nil
        if isRunning { status = "Running" }
        else if status == "Running" || status == "Checking" { status = "Idle" }
    }
    public mutating func apply(events: [AgentEvent]) {
        for event in events {
            latestCursor = max(latestCursor, event.cursor)
            // A state read may already include these events. It owns active-turn
            // membership until the replay catches up to that read's cursor.
            if event.cursor > stateCursor {
                if event.type == "turn_accepted", !activeTurns.contains(event.turnID) { activeTurns.append(event.turnID) }
                if ["turn_completed", "turn_cancelled", "turn_failed"].contains(event.type) { activeTurns.removeAll { $0 == event.turnID } }
                stateCursor = event.cursor
            }
            if ["turn_completed", "turn_cancelled", "turn_failed"].contains(event.type), event.cursor == latestCursor, !isRunning {
                status = event.type == "turn_completed" ? "Ready" : event.type == "turn_failed" ? "Failed" : "Stopped"
            }
        }
        if isRunning { status = "Running" }
        if let position = events.last?.cursor, position >= previewCursor,
           let row = transcript(events).last(where: { $0.role != "You" && !$0.text.isEmpty }) {
            previewCursor = position; preview = String(row.text.suffix(1400))
        }
    }
}

/// Card identity is pinned while live updates arrive. Only user navigation moves it.
public struct InboxDeck: Sendable {
    public private(set) var order: [String] = []
    public private(set) var focusedID: String?
    public private(set) var seen: [String: Cursor] = [:]
    private var previous: [String] = []
    public init() {}
    public mutating func reconcile(_ ids: [String]) {
        let allowed = Set(ids)
        order = order.filter { allowed.contains($0) }
        let existing = Set(order)
        order.append(contentsOf: ids.filter { !existing.contains($0) })
        if focusedID == nil || !allowed.contains(focusedID!) { focusedID = order.first }
        previous = previous.filter { allowed.contains($0) }
    }
    public mutating func prioritize(_ ids: [String]) {
        // Rank the next cards without changing the card currently being read.
        let known = Set(order)
        order = ids.filter { known.contains($0) }
    }
    public mutating func focus(_ id: String) { if order.contains(id) { focusedID = id } }
    public mutating func advance(reviewed: Cursor? = nil) {
        guard let id = focusedID, let index = order.firstIndex(of: id) else { return }
        if let reviewed { seen[id] = reviewed }
        previous.append(id); if previous.count > 50 { previous.removeFirst() }
        order.remove(at: index); order.append(id)
        focusedID = order.first
    }
    public mutating func back() { if let id = previous.popLast() { focusedID = id } }
    public var canGoBack: Bool { !previous.isEmpty }
}
