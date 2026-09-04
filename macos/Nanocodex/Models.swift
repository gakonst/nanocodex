import Foundation

indirect enum JSONValue: Codable, Equatable, Sendable {
    case object([String: JSONValue]), array([JSONValue]), string(String), number(Double), bool(Bool), null
    init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() { self = .null }
        else if let v = try? c.decode(Bool.self) { self = .bool(v) }
        else if let v = try? c.decode(String.self) { self = .string(v) }
        else if let v = try? c.decode(Double.self) { self = .number(v) }
        else if let v = try? c.decode([String: JSONValue].self) { self = .object(v) }
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
}
struct TabLayout: Codable, Equatable, Sendable {
    var tabs: [WorkspaceTab] = [WorkspaceTab()]
    var activeTabId = ""
    var tabPosition = "left"
    var theme = "system"
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
    var status: String?
    var error: String?
    var calls: Int?
    var activeCalls: Int?
    var logs: [String]?
    var isRunning: Bool { status == "connected" || status == "connecting" }
}
struct DesktopState: Decodable, Sendable {
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
    var hasAcceptedTurn: Bool {
        (acceptedTurns ?? 0) > 0 || !activeTurns.isEmpty || events.contains { $0.data["type"].string == "turn_accepted" }
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
    var status = ""
    var streaming = false
    var agent: String?
}

/// Project the durable event protocol into a transcript without duplicating replayed deltas.
func projectTimeline(_ events: [ManagedEvent]) -> [MessageEntry] {
    var rows: [MessageEntry] = []
    var seen = Set<String>()
    for envelope in events where seen.insert(envelope.cursor).inserted {
        let d = envelope.data, turn = envelope.turnId ?? d["id"].string
        let id = "\(turn):\(envelope.cursor)"
        switch d["type"].string {
        case "turn_accepted":
            let input = d["input"]
            let text = input.array.isEmpty ? input.string : input.array.map { $0["text"].string.isEmpty ? "[Attachment]" : $0["text"].string }.joined(separator: "\n")
            rows.append(.init(id: id, turnId: turn, kind: .user, text: text))
        case "turn_completed":
            let final = d["final_message"].string
            if !final.isEmpty, rows.last(where: { $0.turnId == turn && $0.kind == .assistant })?.text != final {
                rows.append(.init(id: id, turnId: turn, kind: .assistant, text: final))
            }
            for index in rows.indices where rows[index].turnId == turn { rows[index].streaming = false }
        case "turn_failed", "turn_cancelled":
            rows.append(.init(id: id, turnId: turn, kind: .error, text: d["type"].string == "turn_cancelled" ? "Stopped by you." : d["error"].string))
            for index in rows.indices where rows[index].turnId == turn {
                rows[index].streaming = false
                if rows[index].status == "running" { rows[index].status = "cancelled" }
            }
        case "event":
            let event = d["event"], p = event["payload"], type = event["type"].string
            let agent = d["agent_id"] == .null ? nil : d["agent_id"].pretty
            let toolID = "\(turn):\(agent ?? "root"):tool:\(p["call_id"].string)"
            switch type {
            case "assistant.delta", "reasoning.summary.delta":
                let kind: MessageEntry.Kind = type == "assistant.delta" ? .assistant : .reasoning
                if let last = rows.indices.last, rows[last].turnId == turn, rows[last].kind == kind, rows[last].agent == agent, rows[last].streaming {
                    rows[last].text += p["text"].string
                } else { rows.append(.init(id: id, turnId: turn, kind: kind, text: p["text"].string, streaming: true, agent: agent)) }
            case "assistant.message":
                if let last = rows.indices.last, rows[last].turnId == turn, rows[last].kind == .assistant, rows[last].agent == agent, rows[last].streaming {
                    rows[last].text = p["text"].string; rows[last].streaming = false
                } else if !p["text"].string.isEmpty { rows.append(.init(id: id, turnId: turn, kind: .assistant, text: p["text"].string, agent: agent)) }
            case "tool.call":
                rows.append(.init(id: toolID, turnId: turn, kind: .tool, text: p["arguments"].pretty, name: p["tool"].string, status: "running", agent: agent))
            case "tool.result":
                if let index = rows.firstIndex(where: { $0.id == toolID }) {
                    rows[index].output = (p["structured_result"] == .null ? p["result"] : p["structured_result"]).pretty
                    rows[index].status = p["status"].string
                }
            case "run.steered": rows.append(.init(id: id, turnId: turn, kind: .notice, text: "Direction updated"))
            case "run.error":
                if p["message"].string != "the turn was cancelled" { rows.append(.init(id: id, turnId: turn, kind: .error, text: p["message"].string)) }
            default: break
            }
        default: break
        }
    }
    return rows
}
