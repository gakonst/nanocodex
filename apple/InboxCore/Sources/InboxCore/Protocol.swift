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
    /// Immutable tool results are prepared once when admitted, not on every
    /// streamed transcript rebuild (which may include large generated images).
    let preparedToolResult: ToolPresentation?
    public var type: String { data["type"].string }
    public var turnID: String { data["turn_id"].string.isEmpty ? data["id"].string : data["turn_id"].string }
    public init(_ data: JSON, cursor: String? = nil) throws {
        guard let position = Cursor(rawValue: cursor ?? data["cursor"].string), !data["type"].string.isEmpty else { throw APIError.invalidResponse }
        self.cursor = position; self.data = data
        let event = data["event"], payload = event["payload"]
        if data["type"].string == "event", event["type"].string == "tool.result" {
            var result = ToolPresentation(name: payload["tool"].string, arguments: .null, metadata: payload["metadata"])
            let preferred = payload["structured_result"] == .null ? payload["result"] : payload["structured_result"]
            result.finish(preferred, failed: payload["is_error"].bool || payload["isError"].bool,
                state: payload["status"].string, metadata: payload["metadata"], rawResult: payload["result"])
            preparedToolResult = result
        } else { preparedToolResult = nil }
    }
}

public struct SSEFrame: Sendable {
    public var event: AgentEvent?
    public var cursor: Cursor?
    public var payloadBytes = 0
}

/// Byte parsing preserves empty lines and split UTF-8/CRLF boundaries. Frames, including multiline data,
/// are committed only at the empty-line boundary; heartbeats never become text.
public struct SSEParser: Sendable {
    private var lines: [String] = []
    private var lineBytes = Data()
    private var previousWasCR = false
    public init() {}
    public mutating func append(byte: UInt8) throws -> SSEFrame? {
        if previousWasCR && byte == 10 { previousWasCR = false; return nil }
        previousWasCR = byte == 13
        if byte == 10 || byte == 13 {
            guard let line = String(data: lineBytes, encoding: .utf8) else { throw APIError.invalidResponse }
            lineBytes.removeAll(keepingCapacity: false)
            return try append(line: line)
        }
        lineBytes.append(byte)
        return nil
    }
    public mutating func append(line: String) throws -> SSEFrame? {
        guard line.isEmpty else { lines.append(line); return nil }
        defer { lines.removeAll(keepingCapacity: true) }
        var id: String?, control: Cursor?, data: [String] = [], heartbeat = false
        for line in lines {
            if line.hasPrefix(": cursor ") { control = Cursor(rawValue: String(line.dropFirst(9))); continue }
            if line == ": keepalive" { heartbeat = true; continue }
            let pair = line.split(separator: ":", maxSplits: 1, omittingEmptySubsequences: false)
            var value = pair.count == 2 ? String(pair[1]) : ""
            if value.hasPrefix(" ") { value.removeFirst() }
            if pair[0] == "id", Cursor(rawValue: value) != nil { id = value }
            if pair[0] == "data" { data.append(value) }
        }
        if data.isEmpty { return control != nil || heartbeat ? SSEFrame(cursor: control) : nil }
        let payload = Data(data.joined(separator: "\n").utf8)
        let json = try JSONDecoder().decode(JSON.self, from: payload)
        let event = try AgentEvent(json, cursor: id)
        return SSEFrame(event: event, cursor: event.cursor, payloadBytes: payload.count)
    }
}

public struct TranscriptRow: Identifiable, Codable, Equatable, Sendable {
    public let id: String
    public var role: String
    public var text: String
    public var detail: String = ""
    public var running = false
    public var tool: ToolPresentation?
    public var images: [String]?
    public var videos: [TranscriptVideo]?
    public var imageFiles: [MessageAttachment]?
    public var turnID: String?
    public var agentID: String?
    public var phase: String?
    public var itemID: String?
    /// Cursor that admitted this row; retained while streamed content changes.
    public var cursor: Cursor?
    public init(id: String, role: String, text: String, detail: String = "", running: Bool = false, tool: ToolPresentation? = nil, images: [String]? = nil) {
        self.id = id; self.role = role; self.text = text; self.detail = detail; self.running = running; self.tool = tool; self.images = images
    }
}

/// Same durable envelope vocabulary as the existing macOS client (PR #256).
/// Stream identity includes both the turn and subagent to prevent mixed output.
public struct TranscriptProjection: Sendable {
    public private(set) var rows: [TranscriptRow] = []
    private var seen = Set<String>()
    private var seenToolCalls = Set<String>()
    private var seenToolResults = Set<String>()
    private var terminalSessions: [String: Int] = [:]
    private var terminalPolls: [String: Int] = [:]
    private var toolStartedAt: [String: Double] = [:]
    private struct StreamRole: Hashable {
        let turn: String
        let agent: String
        let role: String
    }
    private var turnRows: [String: [Int]] = [:]
    private var lastStreamRow: [StreamRole: Int] = [:]
    private var lastUserRow: [String: Int] = [:]
    private var lastFinalRow: [String: Int] = [:]
    private var toolRows: [String: Int] = [:]
    public init() {}

    private mutating func finish(_ turn: String, cancelled: Bool) {
        for index in turnRows[turn] ?? [] {
            if rows[index].running, rows[index].tool != nil {
                rows[index].tool?.status = cancelled ? "Stopped" : "Result unavailable"
            }
            rows[index].running = false
        }
    }

    private func elapsedSeconds(_ id: String, at value: JSON) -> Double? {
        guard let start = toolStartedAt[id], case .number(let end) = value,
              end.isFinite, end >= start else { return nil }
        return (end - start) / 1_000
    }
    public mutating func append(_ events: ArraySlice<AgentEvent>) {
        for envelope in events where seen.insert(envelope.cursor.rawValue).inserted {
            let d = envelope.data, turn = envelope.turnID, agent = envelope.data["agent_id"].pretty
            let firstNewRow = rows.count
            let prefix = turn + ":" + agent
            let id = prefix + ":" + envelope.cursor.rawValue
            if envelope.type == "turn_accepted" {
                let input = d["input"]
                let fileImages = ImageAttachmentContent.project(input.array)
                let media = VideoAttachmentContent.project(fileImages.remaining)
                let text = input.array.isEmpty ? input.string : media.remaining.compactMap {
                    $0["type"].string == "image" ? nil : $0["type"].string == "audio" ? "[Audio]" : $0["text"].string
                }.joined(separator: "\n")
                let images = media.remaining.filter { $0["type"].string == "image" }.map { $0["image_url"].string }
                if let spoken = RealtimeTranscript.project(text) {
                    for (index, entry) in spoken.enumerated() {
                        rows.append(.init(id: id + ":voice:\(index)", role: entry.speaker == "user" ? "You" : "Agent", text: entry.text))
                    }
                } else {
                    var row = TranscriptRow(id: id, role: "You", text: text, images: images.isEmpty ? nil : images)
                    row.videos = media.videos.isEmpty ? nil : media.videos
                    row.imageFiles = fileImages.images.isEmpty ? nil : fileImages.images
                    rows.append(row)
                }
            } else if envelope.type == "turn_completed" {
                let final = d["final_message"].string
                if !final.isEmpty {
                    if let last = lastFinalRow[turn], last > (lastUserRow[turn] ?? -1) {
                        rows[last].text = final; rows[last].phase = "final_answer"
                    } else if let index = turnRows[turn]?.last(where: { rows[$0].agentID == nil && rows[$0].role == "Agent" && rows[$0].text == final }) {
                        rows[index].phase = "final_answer"
                        lastFinalRow[turn] = max(lastFinalRow[turn] ?? -1, index)
                    } else {
                        var row = TranscriptRow(id: id, role: "Agent", text: final)
                        row.phase = "final_answer"; rows.append(row)
                    }
                }
                finish(turn, cancelled: envelope.type == "turn_cancelled")
            } else if envelope.type == "turn_failed" || envelope.type == "turn_cancelled" {
                rows.append(.init(id: id, role: "Status", text: envelope.type == "turn_cancelled" ? "Stopped." : (d["error"].string.isEmpty ? "This turn failed. Open its activity for details." : d["error"].string)))
                finish(turn, cancelled: envelope.type == "turn_cancelled")
            } else if envelope.type == "event" {
                let event = d["event"], p = event["payload"], type = event["type"].string
                let role = type == "reasoning.summary.delta" ? "Thinking" : "Agent"
                let phase = p["phase"].string.isEmpty ? nil : p["phase"].string
                let itemID = p["item_id"].string.isEmpty ? nil : p["item_id"].string
                switch type {
                case "assistant.delta", "reasoning.summary.delta":
                    if let last = lastStreamRow[.init(turn: turn, agent: agent, role: role)], rows[last].running,
                       rows[last].phase == phase, rows[last].itemID == itemID {
                        rows[last].text += p["text"].string
                    } else { rows.append(.init(id: id, role: role, text: p["text"].string, running: true)) }
                case "assistant.message":
                    if let last = lastStreamRow[.init(turn: turn, agent: agent, role: "Agent")], rows[last].running,
                       (phase == nil || rows[last].phase == phase), (itemID == nil || rows[last].itemID == itemID) {
                        if !p["text"].string.isEmpty { rows[last].text = p["text"].string }
                        rows[last].running = false
                    } else { rows.append(.init(id: id, role: "Agent", text: p["text"].string)) }
                case "tool.call":
                    let toolID = prefix + ":tool:" + p["call_id"].string
                    guard seenToolCalls.insert(toolID).inserted, !seenToolResults.contains(toolID) else { continue }
                    if case .number(let time) = d["created_at"], time.isFinite, time >= 0 {
                        toolStartedAt[toolID] = time
                    }
                    if p["tool"].string == "write_stdin",
                       let index = terminalSessions[agent + ":" + p["arguments"]["session_id"].pretty] {
                        terminalPolls[prefix + ":tool:" + p["call_id"].string] = index
                        if p["arguments"]["chars"].string.isEmpty { continue }
                    }
                    let tool = ToolPresentation(name: p["tool"].string, arguments: p["arguments"], metadata: p["metadata"])
                    rows.append(.init(id: prefix + ":tool:" + p["call_id"].string, role: "Tool", text: tool.title, running: true, tool: tool))
                case "tool.result":
                    let toolID = prefix + ":tool:" + p["call_id"].string
                    guard seenToolResults.insert(toolID).inserted else { continue }
                    guard let result = envelope.preparedToolResult else { break }
                    if result.terminalCommand == true {
                        let result = p["structured_result"] == .null ? p["result"] : p["structured_result"]
                        if let index = terminalPolls.removeValue(forKey: toolID) {
                            let previous = rows[index].tool?.output.first(where: { $0.label == "Output" })?.value ?? ""
                            let decoded = ToolPresentation.decoded(result)
                            var combined: [String: JSON]
                            if case .object(let object) = decoded { combined = object }
                            else { combined = ["output": decoded] }
                            let output = previous + combined["output", default: .null].string
                            combined["output"] = .string(output)
                            let elapsed = elapsedSeconds(rows[index].id, at: d["created_at"])
                            rows[index].tool?.finish(.object(combined), failed: p["is_error"].bool || p["isError"].bool, state: p["status"].string, metadata: p["metadata"], rawResult: p["result"], elapsedSeconds: elapsed)
                            rows[index].running = rows[index].tool?.status == "Running"
                            if toolRows[toolID] == nil { continue }
                        }
                        if let index = toolRows[toolID] {
                            let elapsed = elapsedSeconds(toolID, at: d["created_at"])
                            rows[index].tool?.finish(result, failed: p["is_error"].bool || p["isError"].bool, state: p["status"].string, metadata: p["metadata"], rawResult: p["result"], elapsedSeconds: elapsed)
                            rows[index].running = rows[index].tool?.status == "Running"
                            let session = ToolPresentation.decoded(result)["session_id"]
                            if p["tool"].string == "exec_command", session != .null {
                                terminalSessions[agent + ":" + session.pretty] = index
                            }
                        } else {
                            // History can start after a call. Its result must remain readable.
                            var tool = ToolPresentation(name: p["tool"].string, arguments: .null, metadata: p["metadata"])
                            tool.finish(result, failed: p["is_error"].bool || p["isError"].bool, state: p["status"].string, metadata: p["metadata"], rawResult: p["result"])
                            rows.append(.init(id: toolID, role: "Tool", text: tool.title, running: tool.status == "Running", tool: tool))
                        }
                    } else {
                        if let index = toolRows[toolID] {
                            rows[index].running = result.status == "Running"
                            rows[index].tool?.applyCompletion(result, metadata: p["metadata"])
                        } else {
                            // History can start after a call. Its result must remain readable.
                            rows.append(.init(id: toolID, role: "Tool", text: result.title, running: result.status == "Running", tool: result))
                        }
                    }
                case "run.steered": rows.append(.init(id: id, role: "Status", text: "Direction updated"))
                case "run.error": rows.append(.init(id: id, role: "Status", text: p["message"].string))
                default: break
                }
                for index in firstNewRow..<rows.count { rows[index].phase = phase; rows[index].itemID = itemID }
            }
            for index in firstNewRow..<rows.count {
                rows[index].cursor = envelope.cursor
                rows[index].turnID = turn
                rows[index].agentID = agent.isEmpty ? nil : agent
                turnRows[turn, default: []].append(index)
                lastStreamRow[.init(turn: turn, agent: agent, role: rows[index].role)] = index
                if rows[index].role == "You" { lastUserRow[turn] = index }
                if rows[index].role == "Agent", agent.isEmpty,
                   rows[index].phase == nil || rows[index].phase == "final_answer" { lastFinalRow[turn] = index }
                if rows[index].role == "Tool", toolRows[rows[index].id] == nil { toolRows[rows[index].id] = index }
            }
        }
    }
}

public func transcript(_ events: [AgentEvent]) -> [TranscriptRow] {
    var projection = TranscriptProjection()
    projection.append(events[...])
    return projection.rows
}
