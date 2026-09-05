import Foundation

public indirect enum JSON: Codable, Equatable, Sendable {
    case object([String: JSON]), array([JSON]), string(String), number(Double), bool(Bool), null
    public init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() { self = .null }
        else if let v = try? c.decode(Bool.self) { self = .bool(v) }
        else if let v = try? c.decode(String.self) { self = .string(v) }
        else if let v = try? c.decode(Double.self) { self = .number(v) }
        else if let v = try? c.decode([String: JSON].self) { self = .object(v) }
        else { self = .array(try c.decode([JSON].self)) }
    }
    public func encode(to encoder: Encoder) throws {
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
    public subscript(_ key: String) -> JSON { if case .object(let v) = self { return v[key] ?? .null }; return .null }
    public var string: String { if case .string(let v) = self { return v }; return "" }
    public var array: [JSON] { if case .array(let v) = self { return v }; return [] }
    public var number: Double { if case .number(let v) = self { return v }; return 0 }
    public var bool: Bool { if case .bool(let v) = self { return v }; return false }
    public var pretty: String {
        if case .string(let v) = self { return v }
        if self == .null { return "" }
        let encoder = JSONEncoder(); encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        return String(data: (try? encoder.encode(self)) ?? Data(), encoding: .utf8) ?? ""
    }
}

/// Decimal cursor comparison must not round through a Double, including above 2^53.
public struct Cursor: RawRepresentable, Codable, Equatable, Comparable, Sendable {
    public let rawValue: String
    public init?(rawValue: String) {
        guard rawValue == "0" || (rawValue.first != "0" && !rawValue.isEmpty && rawValue.utf8.allSatisfy({ (48...57).contains($0) })) else { return nil }
        self.rawValue = rawValue
    }
    public static let zero = Cursor(rawValue: "0")!
    public static func < (lhs: Self, rhs: Self) -> Bool {
        lhs.rawValue.count == rhs.rawValue.count ? lhs.rawValue < rhs.rawValue : lhs.rawValue.count < rhs.rawValue.count
    }
}

public struct AgentEvent: Equatable, Sendable {
    public let cursor: Cursor
    public let data: JSON
    public var type: String { data["type"].string }
    public var turnID: String { data["turn_id"].string.isEmpty ? data["id"].string : data["turn_id"].string }
    public init(_ data: JSON, cursor: String? = nil) throws {
        guard let position = Cursor(rawValue: cursor ?? data["cursor"].string), !data["type"].string.isEmpty else { throw APIError.invalidResponse }
        self.cursor = position; self.data = data
    }
}

public struct SSEFrame: Sendable {
    public var event: AgentEvent?
    public var cursor: Cursor?
}

/// Byte parsing preserves empty lines and split UTF-8/CRLF boundaries. Frames, including multiline data,
/// are committed only at the empty-line boundary; heartbeats never become text.
public struct SSEParser: Sendable {
    private var lines: [String] = []
    private var size = 0
    private var lineBytes = Data()
    private var previousWasCR = false
    public init() {}
    public mutating func append(byte: UInt8) throws -> SSEFrame? {
        if previousWasCR && byte == 10 { previousWasCR = false; return nil }
        previousWasCR = byte == 13
        if byte == 10 || byte == 13 {
            guard let line = String(data: lineBytes, encoding: .utf8) else { throw APIError.invalidResponse }
            lineBytes.removeAll(keepingCapacity: true)
            return try append(line: line)
        }
        guard lineBytes.count + size < 16 * 1024 * 1024 else { throw APIError.invalidResponse }
        lineBytes.append(byte)
        return nil
    }
    public mutating func append(line: String) throws -> SSEFrame? {
        size += line.utf8.count + 1
        guard size <= 16 * 1024 * 1024 else { throw APIError.invalidResponse }
        guard line.isEmpty else { lines.append(line); return nil }
        defer { lines.removeAll(keepingCapacity: true); size = 0 }
        var id: String?, control: Cursor?, data: [String] = []
        for line in lines {
            if line.hasPrefix(": cursor ") { control = Cursor(rawValue: String(line.dropFirst(9))); continue }
            let pair = line.split(separator: ":", maxSplits: 1, omittingEmptySubsequences: false)
            var value = pair.count == 2 ? String(pair[1]) : ""
            if value.hasPrefix(" ") { value.removeFirst() }
            if pair[0] == "id", Cursor(rawValue: value) != nil { id = value }
            if pair[0] == "data" { data.append(value) }
        }
        if data.isEmpty { return control.map { SSEFrame(cursor: $0) } }
        let json = try JSONDecoder().decode(JSON.self, from: Data(data.joined(separator: "\n").utf8))
        let event = try AgentEvent(json, cursor: id)
        return SSEFrame(event: event, cursor: event.cursor)
    }
}

public struct TranscriptRow: Identifiable, Equatable, Sendable {
    public let id: String
    public var role: String
    public var text: String
    public var detail: String = ""
    public var running = false
    public init(id: String, role: String, text: String, detail: String = "", running: Bool = false) {
        self.id = id; self.role = role; self.text = text; self.detail = detail; self.running = running
    }
}

/// Same durable envelope vocabulary as the existing macOS client (PR #256).
/// Stream identity includes both the turn and subagent to prevent mixed output.
public func transcript(_ events: [AgentEvent]) -> [TranscriptRow] {
    var rows: [TranscriptRow] = []
    var seen = Set<String>()
    for envelope in events where seen.insert(envelope.cursor.rawValue).inserted {
        let d = envelope.data, turn = envelope.turnID
        let prefix = turn + ":" + d["agent_id"].pretty
        let id = prefix + ":" + envelope.cursor.rawValue
        if envelope.type == "turn_accepted" {
            let input = d["input"]
            let text = input.array.isEmpty ? input.string : input.array.map { $0["text"].string.isEmpty ? "[Attachment]" : $0["text"].string }.joined(separator: "\n")
            rows.append(.init(id: id, role: "You", text: text))
        } else if envelope.type == "turn_completed" {
            let final = d["final_message"].string
            if !final.isEmpty, rows.last(where: { $0.id.hasPrefix(turn + "::") && $0.role == "Agent" })?.text != final {
                rows.append(.init(id: id, role: "Agent", text: final))
            }
            for index in rows.indices where rows[index].id.hasPrefix(turn + ":") { rows[index].running = false }
        } else if envelope.type == "turn_failed" || envelope.type == "turn_cancelled" {
            rows.append(.init(id: id, role: "Status", text: envelope.type == "turn_cancelled" ? "Stopped." : d["error"].string))
            for index in rows.indices where rows[index].id.hasPrefix(turn + ":") { rows[index].running = false }
        } else if envelope.type == "event" {
            let event = d["event"], p = event["payload"], type = event["type"].string
            let role = type == "reasoning.summary.delta" ? "Thinking" : "Agent"
            switch type {
            case "assistant.delta", "reasoning.summary.delta":
                if let last = rows.indices.last, rows[last].id.hasPrefix(prefix + ":"), rows[last].role == role, rows[last].running {
                    rows[last].text += p["text"].string
                } else { rows.append(.init(id: id, role: role, text: p["text"].string, running: true)) }
            case "assistant.message":
                if let last = rows.indices.last, rows[last].id.hasPrefix(prefix + ":"), rows[last].role == "Agent", rows[last].running {
                    rows[last].text = p["text"].string; rows[last].running = false
                } else { rows.append(.init(id: id, role: "Agent", text: p["text"].string)) }
            case "tool.call":
                rows.append(.init(id: prefix + ":tool:" + p["call_id"].string, role: "Tool", text: p["tool"].string, detail: p["arguments"].pretty, running: true))
            case "tool.result":
                if let index = rows.firstIndex(where: { $0.id == prefix + ":tool:" + p["call_id"].string }) {
                    rows[index].running = false
                    rows[index].detail = (p["structured_result"] == .null ? p["result"] : p["structured_result"]).pretty
                }
            case "run.steered": rows.append(.init(id: id, role: "Status", text: "Direction updated"))
            case "run.error": rows.append(.init(id: id, role: "Status", text: p["message"].string))
            default: break
            }
        }
    }
    return rows
}
