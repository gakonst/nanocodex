import Foundation

public struct AgentCard: Identifiable, Equatable, Sendable {
    public let id: String
    public var title: String
    public var updatedAt: Double
    public var turnCount: Int
    /// False only when the service knows this conversation has no schedules.
    public var mayHaveScheduledJobs: Bool
    public var activeTurns: [String] = []
    public var stateCursor: Cursor = .zero
    public var latestCursor: Cursor = .zero
    public var status = "Checking"
    private var statusCursor = Cursor.zero
    public var outcomeCursor: Cursor { statusCursor }
    private var terminalStatus: String?
    public var model = ""
    private var previewCursor: Cursor = .zero
    /// Last history event actually projected into the card, excluding newer
    /// state snapshots whose events have not been read yet.
    public var appliedHistoryCursor: Cursor { previewCursor }
    public var preview = ""
    /// Keep the visible exchange together while the focused transcript reloads.
    /// Retain at most the latest user message and reply, not every card's history.
    public private(set) var previewRows: [TranscriptRow] = []
    public var checked = false
    public var error: String?
    /// Local receipt time, independent of the server's history timestamps.
    public private(set) var observedAt: Date?
    public private(set) var activitySummary = "Working"
    public private(set) var activityDetail = ""
    public private(set) var outcomeSummary = ""
    public init(id: String, title: String, updatedAt: Double = 0, turnCount: Int = 0, mayHaveScheduledJobs: Bool = true) {
        self.id = id; self.title = title; self.updatedAt = updatedAt; self.turnCount = turnCount
        self.mayHaveScheduledJobs = mayHaveScheduledJobs
    }
    /// Historical conversations follow server activity, with a stable tie break.
    public static func mostRecentFirst(_ lhs: Self, _ rhs: Self) -> Bool {
        lhs.updatedAt != rhs.updatedAt ? lhs.updatedAt > rhs.updatedAt : lhs.id < rhs.id
    }
    /// Refresh interactive work first, preserving roster order within each
    /// priority and retaining every agent, including unchanged idle ones.
    public static func refreshOrder(_ cards: [Self], focusedID: String?, voiceID: String?, pendingIDs: Set<String>) -> [String] {
        func priority(_ card: Self) -> Int {
            if card.id == focusedID { return 0 }
            if card.id == voiceID { return 1 }
            if pendingIDs.contains(card.id) { return 2 }
            if card.isRunning { return 3 }
            if card.error != nil { return 4 }
            return 5
        }
        return cards.enumerated().sorted {
            let a = priority($0.element), b = priority($1.element)
            return a != b ? a < b : $0.offset < $1.offset
        }.map { $0.element.id }
    }
    public var isRunning: Bool { !activeTurns.isEmpty }
    public func needsAttention(seen: Cursor?) -> Bool {
        checked && !isRunning && (statusCursor == .zero ? latestCursor : statusCursor) > (seen ?? .zero) && (status == "Ready" || status == "Failed")
    }
    public func isInInbox(seen: Cursor?, deferred: Cursor?) -> Bool {
        if let deferred, latestCursor <= deferred { return false }
        // A roster entry alone is not an inbox update. Keep failed initial
        // reads reachable so the user can retry instead of hiding the error.
        return isRunning || (!checked && error != nil) || latestCursor > (seen ?? .zero)
    }
    public mutating func apply(state: JSON) throws {
        guard let cursor = Cursor(rawValue: state["latest_event_cursor"].string), state["agent_id"].string == id,
              case .array = state["active_turns"] else { throw APIError.invalidResponse }
        guard cursor >= stateCursor else { return }
        observedAt = Date()
        let previousTurns = activeTurns
        activeTurns = state["active_turns"].array.map(\.string)
        if previousTurns != activeTurns { activitySummary = "Working"; activityDetail = "" }
        stateCursor = cursor; latestCursor = max(latestCursor, cursor)
        model = state["settings"]["model"].string
        checked = true; error = nil
        if isRunning {
            if status != "Running" { activitySummary = "Working" }
            status = "Running"; terminalStatus = nil
            statusCursor = max(statusCursor, cursor)
        } else if status == "Running" || status == "Checking" { status = terminalStatus ?? "Idle" }
    }
    public mutating func apply(events: [AgentEvent], transcriptRows: [TranscriptRow]? = nil) {
        let previousTurns = activeTurns
        if events.contains(where: { $0.cursor > latestCursor }) { observedAt = Date() }
        for event in events {
            // Replayed history must not become recent merely because it was read.
            if case .number(let time) = event.data["created_at"], time.isFinite, time >= 0 {
                updatedAt = max(updatedAt, time)
            }
            latestCursor = max(latestCursor, event.cursor)
            // A state read may already include these events. It owns active-turn
            // membership until the replay catches up to that read's cursor.
            if event.cursor > stateCursor {
                if event.type == "turn_accepted", !activeTurns.contains(event.turnID) { activeTurns.append(event.turnID) }
                if ["turn_completed", "turn_cancelled", "turn_failed"].contains(event.type) { activeTurns.removeAll { $0 == event.turnID } }
                stateCursor = event.cursor
            }
            // Outcome ordering is independent of internal activity: a state
            // snapshot can include voice/transport events after the last reply.
            if event.cursor >= statusCursor {
                if event.type == "turn_accepted" {
                    statusCursor = event.cursor; terminalStatus = nil; outcomeSummary = ""
                } else if ["turn_completed", "turn_cancelled", "turn_failed"].contains(event.type) {
                    statusCursor = event.cursor
                    terminalStatus = event.type == "turn_completed" ? "Ready" : event.type == "turn_failed" ? "Failed" : "Stopped"
                    outcomeSummary = AgentActivityText.excerpt(event.data[event.type == "turn_completed" ? "final_message" : "error"].string)
                }
            }
        }
        if previousTurns != activeTurns { activitySummary = "Working"; activityDetail = "" }
        if isRunning { status = "Running" }
        else if let terminalStatus { status = terminalStatus }
        else if ["Running", "Ready", "Failed", "Stopped"].contains(status) { status = "Idle" }
        if let position = events.last?.cursor, position >= previewCursor {
            let rows = transcriptRows ?? transcript(events)
            // Only describe the current turn. Never present an old command or
            // a previous answer as work that is happening now.
            if isRunning, let row = rows.last(where: { row in
                row.turnID.map { activeTurns.contains($0) } == true && row.agentID == nil
                    && ["Thinking", "Tool", "Agent"].contains(row.role)
            }) {
                activityDetail = ""
                if let tool = row.tool {
                    activitySummary = tool.status == "Running" ? tool.title : "Working"
                } else if row.role == "Thinking" { activitySummary = "Thinking" }
                else {
                    activitySummary = row.phase == "commentary" ? "Working" : "Writing response"
                    if row.phase == "commentary" { activityDetail = AgentActivityText.excerpt(row.text) }
                }
            }
            let user = rows.lastIndex(where: { $0.role == "You" })
            let reply = rows.dropFirst(user.map { $0 + 1 } ?? 0)
                .last(where: { $0.role == "Agent" && $0.phase != "commentary" && $0.agentID == nil && !$0.text.isEmpty })
            previewRows = [user.map { rows[$0] }, reply].compactMap { $0 }
            previewCursor = position
            // A partial history page may contain only internal activity.
            // Use the same user-facing reply policy as the full card.
            preview = reply.map { String($0.text.suffix(1400)) } ?? ""
        }
    }
}

/// Card identity is pinned while live updates arrive. Only user navigation moves it.
public struct InboxDeck: Equatable, Sendable {
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

/// The mobile tab window uses server activity timestamps in milliseconds.
public enum ConversationWindow {
    public static let pageSize = 24
    public static let duration: TimeInterval = 24 * 60 * 60

    public static func includes(_ card: AgentCard, focusedID: String? = nil,
                                openedIDs: Set<String> = [], now: Date = Date()) -> Bool {
        card.isRunning || card.id == focusedID || openedIDs.contains(card.id)
            || (card.updatedAt.isFinite && card.updatedAt >= (now.timeIntervalSince1970 - duration) * 1000)
    }

    public static func overview(_ cards: [AgentCard], focusedID: String? = nil,
                                openedIDs: Set<String> = [], olderLimit: Int = 0,
                                now: Date = Date()) -> [AgentCard] {
        let ordered = cards.sorted(by: AgentCard.mostRecentFirst)
        let current = Set(ordered.filter {
            includes($0, focusedID: focusedID, openedIDs: openedIDs, now: now)
        }.map(\.id))
        let older = Set(ordered.filter { !current.contains($0.id) }.prefix(max(0, olderLimit)).map(\.id))
        return ordered.filter { current.contains($0.id) || older.contains($0.id) }
    }
}
