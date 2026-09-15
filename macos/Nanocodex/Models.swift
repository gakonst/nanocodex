import Foundation
import InboxCore
import NanocodexUI
import InboxCore

indirect enum JSONValue: Codable, Equatable, Sendable {
    case object([String: JSONValue]), array([JSONValue]), string(String), number(Double), bool(Bool), null
    init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() { self = .null }
        // Managed envelopes are predominantly objects; avoid three failed scalar
        // decodes for every nested event and payload in a history snapshot.
        else if let v = try? c.decode([String: JSONValue].self) { self = .object(v) }
        else if let v = try? c.decode(String.self) { self = .string(v) }
        else if let v = try? c.decode(Bool.self) { self = .bool(v) }
        else if let v = try? c.decode(Double.self) { self = .number(v) }
        else { self = .array(try c.decode([JSONValue].self)) }
    }
    func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .object(let v): try c.encode(v)
        case .array(let v): try c.encode(v)
        case .string(let v): try c.encode(v)
        case .number(let v): try c.encode(v)
        case .bool(let v): try c.encode(v)
        case .null: try c.encodeNil()
        }
    }
    subscript(_ key: String) -> JSONValue { if case .object(let value) = self { return value[key] ?? .null }; return .null }
    var string: String { if case .string(let v) = self { return v }; return "" }
    var array: [JSONValue] { if case .array(let v) = self { return v }; return [] }
    var inboxJSON: InboxCore.JSON {
        // Share string storage for large image inputs instead of serializing
        // their base64 again on every streamed timeline update.
        switch self {
        case .object(let value): return .object(value.mapValues(\.inboxJSON))
        case .array(let value): return .array(value.map(\.inboxJSON))
        case .string(let value): return .string(value)
        case .number(let value): return .number(value)
        case .bool(let value): return .bool(value)
        case .null: return .null
        }
    }
    var pretty: String {
        if case .string(let value) = self {
            guard let data = value.data(using: .utf8), let parsed = try? JSONDecoder().decode(JSONValue.self, from: data) else { return value }
            return parsed.pretty
        }
        if self == .null { return "" }
        let encoder = JSONEncoder(); encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        return String(data: (try? encoder.encode(self)) ?? Data(), encoding: .utf8) ?? ""
    }
    static func encoded<T: Encodable>(_ value: T) throws -> JSONValue { try JSONDecoder().decode(JSONValue.self, from: JSONEncoder().encode(value)) }
    func decode<T: Decodable>(_ type: T.Type) throws -> T { try JSONDecoder().decode(type, from: JSONEncoder().encode(self)) }
}

struct AgentSettings: Codable, Equatable, Sendable {
    var model = "gpt-5.6-sol"
    var thinking = "high"
    var reasoning_mode = "standard"
    var fast_mode = false
    var modelName: String { ["gpt-6-astra": "Astra", "gpt-5.6-sol": "Sol", "gpt-5.6-terra": "Terra", "gpt-5.6-luna": "Luna"][model] ?? model }
    var supportsProReasoning: Bool { model != "gpt-6-astra" }
    var supportsNoReasoning: Bool { model != "gpt-6-astra" }

    /// Normalize only an explicit model change; retained settings keep their values.
    mutating func selectModel(_ value: String) {
        model = value
        if !supportsNoReasoning && thinking == "none" { thinking = "high" }
        if !supportsProReasoning && reasoning_mode == "pro" { reasoning_mode = "standard" }
    }
}
struct AgentThread: Codable, Identifiable, Equatable, Sendable {
    var id: String
    var title: String
    var updatedAt: Double
    var turnCount: Int
}
struct WorkspaceTab: Codable, Identifiable, Equatable, Sendable {
    var id = UUID().uuidString
    var threadId: String?
    var title: String?
    var draft = ""
    var target = ""
    var folder = ""
    var seenCursor: String?
    var deferredCursor: String?
    var draftSettings: AgentSettings?
}
struct TabLayout: Codable, Equatable, Sendable {
    var tabs: [WorkspaceTab] = [WorkspaceTab()]
    var activeTabId = ""
    var tabPosition = "left"
    var theme = "system"
    var workspaceMode: String?
    var paneWidth: Double?
    var tiledTabIDs: [String]?
    var pendingMessages: [PendingMessage]?
    var paneLayouts: [PaneNode]?
}

/// A browser tab is either one agent or a retained tree of agent panes.
/// Leaf IDs are agent tab IDs; split IDs remain stable while resizing/reordering.
struct PaneNode: Codable, Equatable, Sendable, Identifiable {
    var id: String
    var axis: String? = nil
    var fraction: Double = 0.5
    var children: [PaneNode] = []
    var selectedLeaf: String? = nil
    var leaves: [String] { children.isEmpty ? [id] : children.flatMap(\.leaves) }
    static func row(_ ids: [String]) -> PaneNode? {
        guard let first = ids.first else { return nil }
        guard ids.count > 1, let rest = row(Array(ids.dropFirst())) else { return PaneNode(id: first) }
        return PaneNode(id: UUID().uuidString, axis: "horizontal", fraction: 1 / Double(ids.count), children: [PaneNode(id: first), rest])
    }
    func inserting(_ leaf: String, after target: String, axis: String, before: Bool = false) -> PaneNode {
        if children.isEmpty, id == target {
            return PaneNode(id: UUID().uuidString, axis: axis, children: before ? [PaneNode(id: leaf), self] : [self, PaneNode(id: leaf)])
        }
        var copy = self; copy.children = children.map { $0.inserting(leaf, after: target, axis: axis, before: before) }; return copy
    }
    func swapping(_ first: String, _ second: String) -> PaneNode {
        var copy = self
        if children.isEmpty { copy.id = id == first ? second : id == second ? first : id }
        copy.children = children.map { $0.swapping(first, second) }
        if let selectedLeaf { copy.selectedLeaf = selectedLeaf == first ? second : selectedLeaf == second ? first : selectedLeaf }
        return copy
    }
    func replacing(_ old: String, with new: String) -> PaneNode {
        var copy = self
        if children.isEmpty, id == old { copy.id = new }
        copy.children = children.map { $0.replacing(old, with: new) }; return copy
    }
    func removing(_ leaf: String) -> PaneNode? {
        if children.isEmpty { return id == leaf ? nil : self }
        var copy = self; copy.children = children.compactMap { $0.removing(leaf) }
        return copy.children.count == 1 ? copy.children[0] : copy.children.isEmpty ? nil : copy
    }
    func nearestSplit(to leaf: String, axis: String) -> PaneNode? {
        guard children.count == 2, let child = children.first(where: { $0.leaves.contains(leaf) }) else { return nil }
        return child.nearestSplit(to: leaf, axis: axis) ?? (self.axis == axis ? self : nil)
    }
    // Unit rectangles preserve the split topology for spatial keyboard navigation.
    func paneRects(in rect: CGRect = CGRect(x: 0, y: 0, width: 1, height: 1)) -> [(String, CGRect)] {
        guard children.count == 2 else { return [(id, rect)] }
        let horizontal = axis != "vertical"
        let first = CGRect(x: rect.minX, y: rect.minY, width: horizontal ? rect.width * fraction : rect.width, height: horizontal ? rect.height : rect.height * fraction)
        let second = CGRect(x: horizontal ? first.maxX : rect.minX, y: horizontal ? rect.minY : first.maxY, width: horizontal ? rect.width - first.width : rect.width, height: horizontal ? rect.height : rect.height - first.height)
        return children[0].paneRects(in: first) + children[1].paneRects(in: second)
    }
    func neighbor(of leaf: String, toward direction: PaneDock) -> String? {
        let rects = paneRects()
        guard direction != .center, let origin = rects.first(where: { $0.0 == leaf })?.1 else { return nil }
        let horizontal = direction.axis == "horizontal"
        return rects.filter { id, rect in
            guard id != leaf else { return false }
            switch direction {
            case .left: return rect.maxX <= origin.minX + 0.0001 && min(rect.maxY, origin.maxY) > max(rect.minY, origin.minY)
            case .right: return rect.minX >= origin.maxX - 0.0001 && min(rect.maxY, origin.maxY) > max(rect.minY, origin.minY)
            case .top: return rect.maxY <= origin.minY + 0.0001 && min(rect.maxX, origin.maxX) > max(rect.minX, origin.minX)
            case .bottom: return rect.minY >= origin.maxY - 0.0001 && min(rect.maxX, origin.maxX) > max(rect.minX, origin.minX)
            case .center: return false
            }
        }.min { a, b in
            func score(_ rect: CGRect) -> CGFloat {
                let along = horizontal ? abs(rect.midX - origin.midX) : abs(rect.midY - origin.midY)
                let across = horizontal ? abs(rect.midY - origin.midY) : abs(rect.midX - origin.midX)
                return along + across * 0.25
            }
            return score(a.1) < score(b.1)
        }?.0
    }
    func resizing(_ split: String, to value: Double) -> PaneNode {
        var copy = self
        if id == split { copy.fraction = min(0.85, max(0.15, value)) }
        copy.children = children.map { $0.resizing(split, to: value) }; return copy
    }
}

enum PaneDock: String, CaseIterable {
    case left, right, top, bottom, center
    var axis: String { self == .top || self == .bottom ? "vertical" : "horizontal" }
    var before: Bool { self == .left || self == .top }
    var label: String {
        switch self {
        case .left: "Move to left"
        case .right: "Move to right"
        case .top: "Move above"
        case .bottom: "Move below"
        case .center: "Swap panes"
        }
    }
    static func destination(at point: CGPoint, size: CGSize) -> PaneDock {
        let x = point.x / max(1, size.width), y = point.y / max(1, size.height)
        let edges: [(PaneDock, CGFloat)] = [(.left, x), (.right, 1 - x), (.top, y), (.bottom, 1 - y)]
        let nearest = edges.min { $0.1 < $1.1 }!
        return nearest.1 < 0.25 ? nearest.0 : .center
    }
}

/// Matches Inbox's durable queue: submitting once and stopping its captured
/// predecessor are separate operations. Retrying always preserves the payload.
struct PendingMessage: Codable, Identifiable, Equatable, Sendable {
    enum Phase: String, Codable, Sendable { case submitting, queued, starting, cancelling, failed }
    var id = UUID().uuidString
    var tabID: String
    var agentID: String?
    var text: String
    var predecessor = ""
    var phase: Phase = .submitting
    var acceptedCursor: String?
    var error: String?
    var prompt: String?
    var target = ""
    var folder = ""
    var settings: AgentSettings?
    var interruption: String? {
        phase == .queued && !predecessor.isEmpty && predecessor != id ? predecessor : nil
    }
    func hasStarted(in events: [ManagedEvent]) -> Bool {
        events.contains {
            guard ($0.turnId ?? $0.data["id"].string) == id else { return false }
            if ["turn_completed", "turn_failed", "turn_cancelled"].contains($0.data["type"].string) { return true }
            return $0.data["type"].string == "event" && ["run.started", "assistant.delta", "assistant.message", "reasoning.summary.delta", "tool.call", "tool.result"].contains($0.data["event"]["type"].string)
        }
    }
    func hasFinished(in snapshot: ThreadSnapshot) -> Bool {
        guard let acceptedCursor, let cursor = snapshot.cursor, !cursorIsNewer(acceptedCursor, than: cursor) else { return false }
        return !snapshot.activeTurns.contains(id)
    }
    mutating func restore() {
        if phase == .submitting { phase = .failed; error = "Delivery unconfirmed. Retry uses the same message ID." }
        if phase == .starting || phase == .cancelling { phase = .queued }
    }
}

enum WorkspaceFilter: String, CaseIterable { case inbox = "Inbox", running = "Running", all = "All" }

/// Cursors are decimal strings and may exceed both Double and UInt64 precision.
func cursorIsNewer(_ cursor: String, than previous: String?) -> Bool {
    func normalized(_ value: String) -> String {
        let digits = value.drop(while: { $0 == "0" })
        return digits.isEmpty ? "0" : String(digits)
    }
    let lhs = normalized(cursor), rhs = normalized(previous ?? "0")
    return lhs.count == rhs.count ? lhs > rhs : lhs.count > rhs.count
}

struct WorkspaceUpdate {
    var cursor: String
    var running: Bool
    var checked: Bool
    var failed: Bool
    var completed: Bool
    func needsAttention(_ tab: WorkspaceTab) -> Bool {
        checked && !running && (failed || completed) && cursorIsNewer(cursor, than: tab.seenCursor)
    }
    func isInInbox(_ tab: WorkspaceTab) -> Bool {
        if let deferred = tab.deferredCursor, !cursorIsNewer(cursor, than: deferred) { return false }
        return !checked || running || cursorIsNewer(cursor, than: tab.seenCursor)
    }
}
struct Hand: Codable, Identifiable, Equatable, Sendable {
    var id: String
    var name: String
    var kind: String
    var workspace: String
    var agentId: String?
    var rootfs: String?
    var guestRuntime: String?
    var binary: String?
    var cpus: Int?
    var memoryMiB: Int?
    var network: Bool?
    var gpu: Bool?
    var status: String?
    var error: String?
    var calls: Int?
    var activeCalls: Int?
    var logs: [String]?
    var isRunning: Bool { status == "connected" || status == "connecting" }
}
struct DesktopState: Decodable, Equatable, Sendable {
    var defaultHandEnabled: Bool?
    var accountHands: [AccountHand]?
    var accountHandsError: String?
    var connected = false
    var baseUrl = "https://nanocodex.gakonst.workers.dev"
    var error: String?
    var threads: [AgentThread] = []
    var hands: [Hand] = []
    var layout: TabLayout?
    var defaults: JSONValue = .object([:])
    var platform = "darwin"
    var version = "0.1.0"
    var accountScope: String?
}
struct AccountHand: Decodable, Identifiable, Equatable, Sendable {
    var id: String
    var name: String
    var workspace: String
    var capabilities: [String]
    var status: String
    var isPhone: Bool { capabilities.contains("background_limited") }
    var isConnected: Bool { status == "connected" }
}
struct ManagedEvent: Codable, Equatable, Sendable {
    var cursor: String
    var turnId: String?
    var data: JSONValue
}
struct ThreadSnapshot: Decodable, Sendable {
    var id: String
    var events: [ManagedEvent]
    var hasMore: Bool
    var connected: Bool
    var activeTurns: [String]
    var settings: AgentSettings
    var error: String?
    var acceptedTurns: Int?
    var cursor: String?
    // Local render data is prepared by the runtime worker, never encoded on the wire.
    var presentation: ThreadPresentation? = nil
    private enum CodingKeys: String, CodingKey { case id, events, hasMore, connected, activeTurns, settings, error, acceptedTurns, cursor }
    var hasAcceptedTurn: Bool {
        (acceptedTurns ?? 0) > 0 || !activeTurns.isEmpty || events.contains { $0.data["type"].string == "turn_accepted" }
    }
}

struct ThreadPresentation: Sendable {
    let revision: UUID
    let events: [ManagedEvent]
    let messages: [MessageEntry]
    let displayedMessages: [MessageEntry]
    let terminalType: String?
    let queue: ThreadQueueFacts
}

struct ThreadQueueFacts: Sendable {
    var accepted: [String: String] = [:]
    var started = Set<String>()
    var finished = Set<String>()
    var cancelled: [String] = []
    init(_ events: [ManagedEvent]) {
        for event in events {
            let id = event.turnId ?? event.data["id"].string, type = event.data["type"].string
            if type == "turn_accepted", accepted[id] == nil { accepted[id] = event.cursor }
            if ["turn_completed", "turn_failed", "turn_cancelled"].contains(type) { finished.insert(id); started.insert(id) }
            if type == "turn_cancelled" { cancelled.append(id) }
            if type == "event", ["run.started", "assistant.delta", "assistant.message", "reasoning.summary.delta", "tool.call", "tool.result"].contains(event.data["event"]["type"].string) { started.insert(id) }
        }
    }
}

/// Owned exclusively by the runtime's serial worker. Bound retained reducer
/// state; re-opening an evicted thread rebuilds from its authoritative snapshot.
struct ThreadPresentationCache {
    private struct Entry { var events: [ManagedEvent]; var reducer: TimelineProjection; var presentation: ThreadPresentation }
    private var entries: [String: Entry] = [:]
    private var recent: [String] = []
    mutating func prepare(_ snapshot: ThreadSnapshot) -> ThreadSnapshot {
        var copy = snapshot
        recent.removeAll { $0 == snapshot.id }; recent.append(snapshot.id)
        if let entry = entries[snapshot.id], entry.events == snapshot.events {
            copy.presentation = entry.presentation; return copy
        }
        var reducer = entries[snapshot.id]?.reducer ?? TimelineProjection()
        let events = conversationEvents(snapshot.events)
        let messages = reducer.project(events).map { entry in
            var copy = entry
            copy.preparedActivityTitle = entry.activityTitle
            copy.preparedActivitySubject = entry.activitySubject
            return copy
        }
        let presentation = ThreadPresentation(revision: UUID(), events: events, messages: messages,
                                              displayedMessages: messages.flatMap { $0.expandingVoiceTranscript() },
                                              terminalType: events.last { ["turn_completed", "turn_failed", "turn_cancelled"].contains($0.data["type"].string) }?.data["type"].string,
                                              queue: ThreadQueueFacts(snapshot.events))
        entries[snapshot.id] = Entry(events: snapshot.events, reducer: reducer, presentation: presentation)
        while recent.count > 24 { entries.removeValue(forKey: recent.removeFirst()) }
        copy.presentation = presentation
        return copy
    }
}

struct SignInChallenge: Decodable, Equatable, Sendable {
    var phone: String
    var resendAt: Double
    var expiresAt: Double

    func resendSeconds(at date: Date) -> Int { max(0, Int(ceil(resendAt / 1000 - date.timeIntervalSince1970))) }
    func isExpired(at date: Date) -> Bool { date.timeIntervalSince1970 >= expiresAt / 1000 }
    static func normalizedCode(_ value: String) -> String { String(value.filter { $0 >= "0" && $0 <= "9" }.prefix(6)) }
}
struct MessageEntry: Identifiable, Equatable, Sendable {
    enum Kind: String, Sendable { case user, assistant, reasoning, tool, error, notice }
    var id: String
    var turnId: String
    var kind: Kind
    var text: String
    var name = ""
    var output = ""
    var generatedOutputs: [ChatGeneratedOutput] = []
    var attachments: TranscriptInput?
    var status = ""
    var streaming = false
    var agent: String?
    var phase: String?
    var itemID: String?
    var cursor: String?
    // Prepared labels are a cache, not part of a message's protocol identity.
    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.id == rhs.id && lhs.turnId == rhs.turnId && lhs.kind == rhs.kind && lhs.text == rhs.text &&
        lhs.name == rhs.name && lhs.output == rhs.output && lhs.generatedOutputs == rhs.generatedOutputs &&
        lhs.status == rhs.status && lhs.streaming == rhs.streaming && lhs.agent == rhs.agent &&
        lhs.phase == rhs.phase && lhs.itemID == rhs.itemID && lhs.cursor == rhs.cursor
    }
    var isActivity: Bool { kind == .reasoning || kind == .tool || (kind == .assistant && (phase == "commentary" || agent != nil)) }
    var preparedActivityTitle: String? = nil
    var preparedActivitySubject: String? = nil
    func expandingVoiceTranscript() -> [MessageEntry] {
        guard kind == .user || kind == .assistant, let spoken = RealtimeTranscript.project(text) else { return [self] }
        return spoken.enumerated().map { index, turn in
            MessageEntry(id: id + ":voice:\(index)", turnId: turnId, kind: turn.speaker == "user" ? .user : .assistant,
                         text: turn.text, streaming: streaming, cursor: cursor)
        }
    }
    var activityTitle: String {
        if let preparedActivityTitle { return preparedActivityTitle }
        guard kind == .tool else { return kind == .reasoning ? "Thinking" : "Progress update" }
        let family = name.components(separatedBy: "__").last?.replacingOccurrences(of: "functions.", with: "") ?? name
        let titles = ["exec": "Run code", "exec_command": "Run command", "write_stdin": "Read process output",
                      "read_file": "Read file", "write_file": "Write file", "apply_patch": "Edit files",
                      "search_query": "Search the web", "web_search": "Search the web", "search": "Search",
                      "accountInfo": "Check available Hands", "mount": "Connect a Hand", "spawn_agent": "Delegate task",
                      "wait_agent": "Wait for agent", "list_agents": "Check agents"]
        if let title = titles[family] { return title }
        if family.hasPrefix("user_") { return "Use connected device" }
        let words = family.replacingOccurrences(of: "([a-z])([A-Z])", with: "$1 $2", options: .regularExpression)
            .replacingOccurrences(of: "[_./-]+", with: " ", options: .regularExpression).lowercased()
        return words.isEmpty ? "Tool call" : words.prefix(1).uppercased() + words.dropFirst()
    }
    var activitySubject: String {
        if let preparedActivitySubject { return preparedActivitySubject }
        if kind != .tool { return String(text.split(whereSeparator: \.isNewline).first ?? "").replacingOccurrences(of: "**", with: "") }
        guard let payload = try? JSONDecoder().decode(JSONValue.self, from: Data(text.utf8)) else { return "" }
        return ["title", "description", "path", "file_path", "query", "url", "command", "cmd"]
            .map { payload[$0].string }.first(where: { !$0.isEmpty })?.split(whereSeparator: \.isWhitespace).joined(separator: " ") ?? ""
    }
    var displayText: String {
        guard kind == .user else { return text }
        return ["\n\n[Selected Hand:", "\n\n[Working folder selected in Nanocodex:"].reduce(text) { value, marker in value.components(separatedBy: marker).first ?? value }
    }
    static func userID(_ turnID: String) -> String { "user:" + turnID }
}

struct NativeConversationItem: Identifiable {
    var id: String
    var message: MessageEntry?
    var activity: [MessageEntry] = []
    var generatedOutputs: [ChatGeneratedOutput] = []
    var isRunning = false
    static func group(_ messages: [MessageEntry], working: Bool) -> [Self] {
        guard let turn = messages.first?.turnId else { return [] }
        let activityID = "activity-" + turn
        var result: [Self] = [], index: Int?, emitted = Set<String>()
        for entry in messages {
            if entry.isActivity {
                if let index { result[index].activity.append(entry) }
                else { index = result.count; result.append(.init(id: activityID, activity: [entry])) }
            } else {
                result.append(.init(id: entry.id, message: entry))
                if entry.kind == .user, working, index == nil { index = result.count; result.append(.init(id: activityID)) }
            }
            let outputs = entry.generatedOutputs.filter { emitted.insert($0.id).inserted }
            if !outputs.isEmpty { result.append(.init(id: "output-" + entry.id, generatedOutputs: outputs)) }
        }
        if let index { result[index].isRunning = working && (messages.last?.isActivity == true || messages.last?.kind == .user) }
        return result.filter { $0.message != nil || !$0.activity.isEmpty || !$0.generatedOutputs.isEmpty || $0.isRunning }
    }
}

/// Cancelling a waiting message can emit run.started while the service drains
/// its cancellation. That is not a reply and must not steal the reading position.
func conversationEvents(_ events: [ManagedEvent]) -> [ManagedEvent] {
    var cancelling = Set<String>(), started = Set<String>(), output = Set<String>()
    for envelope in events {
        let id = envelope.turnId ?? envelope.data["id"].string
        if envelope.data["type"].string == "turn_cancelling" { cancelling.insert(id) }
        guard envelope.data["type"].string == "event" else { continue }
        let type = envelope.data["event"]["type"].string
        if type == "run.started", !cancelling.contains(id) { started.insert(id) }
        if ["assistant.delta", "assistant.message", "reasoning.summary.delta", "tool.call", "tool.result"].contains(type) { output.insert(id) }
    }
    let cancelledBeforeWork = cancelling.subtracting(started).subtracting(output)
    guard !cancelledBeforeWork.isEmpty else { return events }
    return events.filter { !cancelledBeforeWork.contains($0.turnId ?? $0.data["id"].string) }
}

/// Project the durable event protocol into a transcript without duplicating replayed deltas.
/// Completed turns retain their projection while another turn streams. Compare
/// exact envelopes so history prepends, corrections and replay still rebuild
/// every affected turn through the canonical protocol reducer below.
struct TimelineProjection {
    private struct Turn { var events: [ManagedEvent]; var rows: [MessageEntry]; var toolOutputs: [String: NativeToolOutputProjection] }
    private var turns: [String: Turn] = [:]

    mutating func project(_ events: [ManagedEvent]) -> [MessageEntry] {
        var grouped: [String: [ManagedEvent]] = [:], order: [String] = [], seen = Set<String>()
        for event in events where seen.insert(event.cursor).inserted {
            let id = event.turnId ?? event.data["id"].string
            if grouped[id] == nil { order.append(id) }
            grouped[id, default: []].append(event)
        }
        var retained: [String: Turn] = [:], rows: [MessageEntry] = []
        for id in order {
            let events = grouped[id] ?? []
            if let previous = turns[id], previous.events == events {
                retained[id] = previous; rows.append(contentsOf: previous.rows)
            } else {
                var outputs = turns[id]?.toolOutputs ?? [:]
                let projected = projectTimeline(events, toolOutputs: &outputs)
                retained[id] = Turn(events: events, rows: projected, toolOutputs: outputs)
                rows.append(contentsOf: projected)
            }
        }
        turns = retained
        return rows
    }
}

func projectTimeline(_ events: [ManagedEvent]) -> [MessageEntry] {
    var toolOutputs: [String: NativeToolOutputProjection] = [:]
    return projectTimeline(events, toolOutputs: &toolOutputs)
}

/// Keep the two protocol result fields intact until the shared media parser has
/// inspected both. An unchanged image result is never parsed again just because
/// another assistant delta arrived in the same turn.
private struct NativeToolOutputProjection {
    var result: JSONValue
    var structured: JSONValue
    var includeText: Bool
    var outputs: [ChatGeneratedOutput]
    var diagnostics: String
    var failed: Bool

    init(result: JSONValue, structured: JSONValue, includeText: Bool) {
        self.result = result; self.structured = structured; self.includeText = includeText
        let values = [result, structured].filter { $0 != .null }
        let sources = values.compactMap { value in (try? JSONEncoder().encode(value)).flatMap { String(data: $0, encoding: .utf8) } }
        outputs = ChatGeneratedOutput.parse(results: sources, includeText: includeText)
        var seen = Set<String>()
        diagnostics = sources.map(ChatGeneratedOutput.sanitizedText).filter { !$0.isEmpty && seen.insert($0).inserted }.joined(separator: "\n\n")
        failed = values.contains { value in
            let decoded = (try? JSONDecoder().decode(JSONValue.self, from: Data(value.string.utf8))) ?? value
            if decoded["isError"] == .bool(true) || decoded["is_error"] == .bool(true) { return true }
            if case .number(let code) = decoded["exit_code"] { return code != 0 }
            return false
        }
    }
}

private func projectTimeline(_ events: [ManagedEvent], toolOutputs: inout [String: NativeToolOutputProjection]) -> [MessageEntry] {
    var rows: [MessageEntry] = []
    var retainedOutputs = Set<String>()
    var seen = Set<String>()
    // Acceptance of a follow-up can interleave with the predecessor's deltas.
    // Project each turn contiguously so that acceptance never splits a reply.
    var order: [String] = []
    var turns: [String: [ManagedEvent]] = [:]
    for event in conversationEvents(events) where seen.insert(event.cursor).inserted {
        let turn = event.turnId ?? event.data["id"].string
        if turns[turn] == nil { order.append(turn) }
        turns[turn, default: []].append(event)
    }
    for envelope in order.flatMap({ turns[$0] ?? [] }) {
        let d = envelope.data, turn = envelope.turnId ?? d["id"].string
        let id = "\(turn):\(envelope.cursor)"
        let firstNewRow = rows.count
        switch d["type"].string {
        case "turn_accepted":
            let input = d["input"]
            let projected = TranscriptInput(input.inboxJSON)
            let media = projected.images.isEmpty && projected.imageFiles.isEmpty && projected.videos.isEmpty ? nil : projected
            rows.append(.init(id: MessageEntry.userID(turn), turnId: turn, kind: .user,
                              text: projected.text, attachments: media))
        case "turn_completed":
            let final = d["final_message"].string
            if !final.isEmpty {
                if let last = rows.lastIndex(where: { $0.turnId == turn && $0.kind == .assistant && $0.agent == nil && ($0.phase == "final_answer" || $0.phase == nil) }),
                   last > (rows.lastIndex(where: { $0.turnId == turn && $0.kind == .user }) ?? -1) {
                    rows[last].text = final
                    rows[last].phase = "final_answer"
                } else if rows.last(where: { $0.turnId == turn && $0.kind == .assistant && $0.agent == nil })?.text != final {
                    rows.append(.init(id: id, turnId: turn, kind: .assistant, text: final, phase: "final_answer"))
                }
            }
            for index in rows.indices where rows[index].turnId == turn {
                rows[index].streaming = false
                if rows[index].status == "running" { rows[index].status = "unavailable" }
            }
        case "turn_failed", "turn_cancelled":
            rows.append(.init(id: id, turnId: turn, kind: .error, text: d["type"].string == "turn_cancelled" ? "Stopped by you." : d["error"].string))
            for index in rows.indices where rows[index].turnId == turn {
                rows[index].streaming = false
                if rows[index].status == "running" { rows[index].status = "cancelled" }
            }
        case "event":
            let event = d["event"], p = event["payload"], type = event["type"].string
            let agent = d["agent_id"] == .null ? nil : d["agent_id"].pretty
            let phase = p["phase"].string.isEmpty ? nil : p["phase"].string
            let itemID = p["item_id"].string.isEmpty ? nil : p["item_id"].string
            let toolID = "\(turn):\(agent ?? "root"):tool:\(p["call_id"].string)"
            switch type {
            case "assistant.delta", "reasoning.summary.delta":
                let kind: MessageEntry.Kind = type == "assistant.delta" ? .assistant : .reasoning
                if let last = rows.lastIndex(where: { $0.turnId == turn && $0.kind == kind && $0.agent == agent }), rows[last].streaming,
                   rows[last].phase == phase, rows[last].itemID == itemID {
                    rows[last].text += p["text"].string
                } else { rows.append(.init(id: id, turnId: turn, kind: kind, text: p["text"].string, streaming: true, agent: agent, phase: phase, itemID: itemID)) }
            case "assistant.message":
                if let last = rows.lastIndex(where: { $0.turnId == turn && $0.kind == .assistant && $0.agent == agent }), rows[last].streaming,
                   (phase == nil || rows[last].phase == phase), (itemID == nil || rows[last].itemID == itemID) {
                    if !p["text"].string.isEmpty { rows[last].text = p["text"].string }
                    rows[last].streaming = false
                } else if !p["text"].string.isEmpty, rows.last?.kind != .assistant || rows.last?.turnId != turn || rows.last?.agent != agent || rows.last?.text != p["text"].string {
                    rows.append(.init(id: id, turnId: turn, kind: .assistant, text: p["text"].string, agent: agent, phase: phase, itemID: itemID))
                }
            case "tool.call":
                let name = [p["metadata"]["tool_name"].string, p["metadata"]["toolName"].string, p["tool"].string].first(where: { !$0.isEmpty }) ?? ""
                rows.append(.init(id: toolID, turnId: turn, kind: .tool, text: p["arguments"].pretty, name: name, status: "running", agent: agent))
            case "tool.result":
                let result = p["result"], structured = p["structured_result"]
                let index = rows.firstIndex(where: { $0.id == toolID })
                let name = index.map { rows[$0].name } ?? p["tool"].string
                let includeText = false // Tool diagnostics stay in Activity.
                let resultID = toolID + ":" + envelope.cursor
                let output: NativeToolOutputProjection
                if let cached = toolOutputs[resultID], cached.result == result, cached.structured == structured, cached.includeText == includeText { output = cached }
                else { output = NativeToolOutputProjection(result: result, structured: structured, includeText: includeText); toolOutputs[resultID] = output }
                retainedOutputs.insert(resultID)
                let failed = p["is_error"] == .bool(true) || p["isError"] == .bool(true) || output.failed
                let status = p["status"].string == "cancelled" ? "cancelled" : failed ? "failed" : p["status"].string.isEmpty ? "completed" : p["status"].string
                if let index {
                    rows[index].output = output.diagnostics
                    rows[index].generatedOutputs = ToolOutputVisibility.isInspection(name: name, arguments: rows[index].text) ? [] : output.outputs
                    rows[index].status = status
                } else {
                    rows.append(.init(id: toolID, turnId: turn, kind: .tool, text: "", name: name, output: output.diagnostics, generatedOutputs: ToolOutputVisibility.isInspection(name: name, arguments: "") ? [] : output.outputs, status: status, agent: agent))
                }
            case "run.steered": rows.append(.init(id: id, turnId: turn, kind: .notice, text: "Direction updated"))
            case "run.error":
                if p["message"].string != "the turn was cancelled" { rows.append(.init(id: id, turnId: turn, kind: .error, text: p["message"].string)) }
            default: break
            }
        default: break
        }
        for index in firstNewRow..<rows.count { rows[index].cursor = envelope.cursor }
    }
    toolOutputs = toolOutputs.filter { retainedOutputs.contains($0.key) }
    return rows
}
