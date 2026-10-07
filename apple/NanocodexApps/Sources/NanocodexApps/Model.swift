import Foundation

public enum AppValue: Codable, Hashable, Sendable {
    case null, bool(Bool), number(Double), string(String), array([AppValue]), object([String: AppValue])
    public init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() { self = .null }
        else if let value = try? c.decode(Bool.self) { self = .bool(value) }
        else if let value = try? c.decode(Double.self) { self = .number(value) }
        else if let value = try? c.decode(String.self) { self = .string(value) }
        else if let value = try? c.decode([AppValue].self) { self = .array(value) }
        else { self = .object(try c.decode([String: AppValue].self)) }
    }
    public func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .null: try c.encodeNil()
        case .bool(let v): try c.encode(v)
        case .number(let v): try c.encode(v)
        case .string(let v): try c.encode(v)
        case .array(let v): try c.encode(v)
        case .object(let v): try c.encode(v)
        }
    }
    public var text: String {
        switch self {
        case .null: return ""
        case .bool(let v): return String(v)
        case .number(let v): return v.rounded() == v && abs(v) < 1e16 ? String(format: "%.0f", v) : String(v)
        case .string(let v): return v
        case .array(let v): return v.map(\.text).joined(separator: ", ")
        case .object: return "Record"
        }
    }
    public var number: Double { if case .number(let n) = self { return n }; return Double(text) ?? 0 }
    public var truth: Bool { switch self { case .bool(let b): return b; case .null: return false; default: return number != 0 || !text.isEmpty && self != .number(0) } }
}

public struct AppDiagnostic: Error, LocalizedError, Equatable, Codable, Sendable {
    public let message: String
    public let line: Int
    public init(_ message: String, line: Int = 0) { self.message = message; self.line = line }
    public var errorDescription: String? { line > 0 ? "Line \(line): \(message)" : message }
}

indirect enum Expr {
    case value(AppValue), variable(String), binding(String), member(Expr, String), index(Expr, Expr)
    case array([Expr]), dictionary([(Expr, Expr)]), unary(String, Expr), binary(String, Expr, Expr)
    case call(Expr, [Argument], Closure?), conditional(Expr, Expr, Expr), interpolation([Expr]), awaitValue(Expr)
}
struct Argument { var label: String?; var value: Expr }
struct Closure { var parameters: [String]; var body: [Statement] }
indirect enum Statement {
    case expression(Expr), variable(String, Expr), assign(Expr, String, Expr), condition(Expr, [Statement], [Statement])
    case forEach(String, Expr, [Statement]), whileLoop(Expr, [Statement]), returnValue(Expr?)
}
struct StateDeclaration { var name: String; var initial: Expr; var persistedKey: String? }
struct FunctionDeclaration { var parameters: [String]; var body: [Statement] }
struct AppProgram {
    var states: [StateDeclaration] = []
    var constants: [String: Expr] = [:]
    var functions: [String: FunctionDeclaration] = [:]
    var records: [String: [String]] = [:]
    var body: [Statement] = []
}

public struct AppNode: Identifiable, Equatable, Codable, Sendable {
    public var id: String
    public var kind: String
    public var text: String
    public var properties: [String: AppValue]
    public var children: [AppNode]
    public var actionID: String?
    public var binding: String?
    public init(id: String, kind: String, text: String = "", properties: [String: AppValue] = [:], children: [AppNode] = [], actionID: String? = nil, binding: String? = nil) {
        self.id = id; self.kind = kind; self.text = text; self.properties = properties; self.children = children; self.actionID = actionID; self.binding = binding
    }
}

public struct NativeAppHost {
    public var loadState: () async throws -> [String: AppValue]
    public var saveState: ([String: AppValue]) async throws -> Void
    public var runAgent: (String) async throws -> String
    /// False only for isolated fixture hosts that never dispatch external agent work.
    public var agentRequestsHaveExternalEffects: Bool
    /// The host can retain external-operation receipts until the enclosing action commits.
    public var beginAction: () -> Void
    public var commitAction: () -> Void
    public init(loadState: @escaping () async throws -> [String: AppValue], saveState: @escaping ([String: AppValue]) async throws -> Void, runAgent: @escaping (String) async throws -> String, agentRequestsHaveExternalEffects: Bool = true, beginAction: @escaping () -> Void = {}, commitAction: @escaping () -> Void = {}) {
        self.loadState = loadState; self.saveState = saveState; self.runAgent = runAgent
        self.agentRequestsHaveExternalEffects = agentRequestsHaveExternalEffects
        self.beginAction = beginAction; self.commitAction = commitAction
    }
}

public struct AppLimits: Sendable {
    public var steps: Int
    public var depth: Int
    public var nodes: Int
    public var collectionCount: Int
    public var agentCalls: Int
    public init(steps: Int = 500_000, depth: Int = 64, nodes: Int = 2_000, collectionCount: Int = 2_000, agentCalls: Int = 3) {
        self.steps = steps; self.depth = depth; self.nodes = nodes; self.collectionCount = collectionCount; self.agentCalls = agentCalls
    }
}

/// One plotted datum. Charts accept `[numbers]` or records/dictionaries with
/// `label`/`x`, `value`/`y` and an optional `series` (or `group`) field.
public struct ChartEntry: Identifiable, Equatable, Sendable {
    public var id: Int { index }
    public var index: Int
    public var label: String
    public var value: Double
    public var series: String

    public init(index: Int, label: String, value: Double, series: String) {
        self.index = index; self.label = label; self.value = value; self.series = series
    }

    /// Returns nil when the value is not an array (children supply the data instead).
    public static func entries(_ data: AppValue) -> [ChartEntry]? {
        guard case .array(let values) = data else { return nil }
        return values.enumerated().map { index, value in
            if case .object(let record) = value {
                let label = (record["label"] ?? record["x"] ?? record["title"] ?? record["name"])?.text ?? String(index + 1)
                let raw = (record["value"] ?? record["y"] ?? record["count"])?.number ?? 0
                let series = (record["series"] ?? record["group"])?.text ?? ""
                return ChartEntry(index: index, label: label, value: raw.isFinite ? raw : 0, series: series)
            }
            let raw = value.number
            return ChartEntry(index: index, label: String(index + 1), value: raw.isFinite ? raw : 0, series: "")
        }
    }
}

/// A Markdown segment of an assistant response: prose, or a fenced
/// ```swift-artifact block rendered inline with the swift-v1 interpreter.
public enum ChatArtifactSegment: Equatable, Sendable {
    case markdown(String)
    /// `complete` is false while the closing fence has not streamed yet.
    case artifact(source: String, complete: Bool)

    public static let languages: Set<String> = ["swift-artifact", "artifact", "nanocodex-artifact", "swiftui-artifact"]

    /// Splits only top-level fences whose info string is an artifact language.
    /// Other fenced code (including nested artifact examples) stays Markdown.
    public static func split(_ text: String) -> [ChatArtifactSegment] {
        guard languages.contains(where: { text.contains("```" + $0) || text.contains("~~~" + $0) }) else {
            return text.isEmpty ? [] : [.markdown(text)]
        }
        var result: [ChatArtifactSegment] = []
        var prose: [Substring] = []
        var artifact: [Substring] = []
        var openFence: (marker: Character, count: Int, isArtifact: Bool)?
        func flushProse() {
            let joined = prose.joined(separator: "\n")
            if !joined.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { result.append(.markdown(joined)) }
            prose = []
        }
        for line in text.split(separator: "\n", omittingEmptySubsequences: false) {
            let trimmed = line.drop(while: { $0 == " " })
            let indent = line.count - trimmed.count
            if let fence = openFence {
                let run = trimmed.prefix(while: { $0 == fence.marker }).count
                let closes = indent < 4 && run >= fence.count && trimmed.dropFirst(run).allSatisfy(\.isWhitespace)
                if fence.isArtifact {
                    if closes {
                        result.append(.artifact(source: artifact.joined(separator: "\n"), complete: true))
                        artifact = []; openFence = nil
                    } else { artifact.append(line) }
                } else {
                    prose.append(line)
                    if closes { openFence = nil }
                }
                continue
            }
            if indent < 4, let marker = trimmed.first, marker == "`" || marker == "~" {
                let run = trimmed.prefix(while: { $0 == marker }).count
                if run >= 3 {
                    let info = trimmed.dropFirst(run).trimmingCharacters(in: .whitespaces).lowercased()
                    let language = info.split(separator: " ").first.map(String.init) ?? ""
                    if languages.contains(language) {
                        flushProse()
                        openFence = (marker, run, true)
                        continue
                    }
                    openFence = (marker, run, false)
                }
            }
            prose.append(line)
        }
        if let fence = openFence, fence.isArtifact {
            result.append(.artifact(source: artifact.joined(separator: "\n"), complete: false))
        } else { flushProse() }
        return result
    }
}
