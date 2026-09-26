import Foundation

/// Account-owned capture and decision projection. A response is a recorded choice,
/// not proof that an external action has completed.
public struct TodoCapture: Identifiable, Equatable, Sendable {
    public let id: String
    public let body: String
    public let watchHint: String
    public let status: String
    public let version: Int
    public let createdAt: String

    public init(_ json: JSON) throws {
        guard !json["id"].string.isEmpty, !json["body"].string.isEmpty,
              let version = Int(exactly: json["version"].number), version > 0,
              ["captured", "watching", "done", "paused"].contains(json["status"].string) else { throw APIError.invalidResponse }
        id = json["id"].string; body = json["body"].string
        watchHint = json["watch_hint"].string; status = json["status"].string
        self.version = version; createdAt = json["created_at"].string
    }
}

public struct TodoDecisionChoice: Identifiable, Equatable, Sendable {
    public let id: String
    public let title: String
    public init(_ json: JSON) throws {
        guard !json["id"].string.isEmpty, !json["title"].string.isEmpty else { throw APIError.invalidResponse }
        id = json["id"].string; title = json["title"].string
    }
}

public struct TodoDecision: Identifiable, Equatable, Sendable {
    public let id: String
    public let title: String
    public let context: String
    public let todoID: String?
    public let sourceLabel: String
    public let sourceURL: URL?
    public let status: String
    public let version: Int
    public let choices: [TodoDecisionChoice]

    public init(_ json: JSON) throws {
        guard !json["id"].string.isEmpty, !json["title"].string.isEmpty,
              let version = Int(exactly: json["version"].number), version > 0,
              ["needs_you", "answered", "resolved", "stale"].contains(json["status"].string),
              case .array(let options) = json["choices"] else { throw APIError.invalidResponse }
        id = json["id"].string; title = json["title"].string
        context = json["context"].string
        todoID = json["todo_id"].string.isEmpty ? nil : json["todo_id"].string
        sourceLabel = json["source_label"].string
        let url = URL(string: json["source_url"].string)
        sourceURL = url?.scheme == "https" ? url : nil
        status = json["status"].string; self.version = version
        choices = try options.map(TodoDecisionChoice.init)
    }
}

public enum TodoFeedFilter: String, CaseIterable, Sendable {
    case all = "All", actionable = "Actionable", ignore = "Ignore"
}

public struct TodoTrace: Identifiable, Equatable, Sendable {
    public let id: Int
    public let outcome: String
    public let classifierOutcome: String
    public let reason: String
    public let decisionID: String?
    public let sender: String
    public let subject: String
    public let sourceURL: URL?
    public let firstAt: Date?
    public let observedAt: Date?

    public init(_ json: JSON) throws {
        guard let traceID = Int(exactly: json["id"].number), traceID > 0 else { throw APIError.invalidResponse }
        id = traceID; outcome = json["outcome"].string
        classifierOutcome = json["classifier_outcome"].string
        reason = json["reason"].string
        decisionID = json["decision_id"].string.isEmpty ? nil : json["decision_id"].string
        sender = json["sender"].string; subject = json["subject"].string
        let url = URL(string: json["source_url"].string)
        sourceURL = url?.scheme == "https" && !(url?.host ?? "").isEmpty
            && url?.user == nil && url?.password == nil ? url : nil
        firstAt = json["first_at"].number > 0 ? Date(timeIntervalSince1970: json["first_at"].number / 1000) : nil
        observedAt = json["observed_at"].number > 0 ? Date(timeIntervalSince1970: json["observed_at"].number / 1000) : nil
    }
    public var isIgnored: Bool { ["no_reply", "filtered"].contains(outcome) || (reason == "low_confidence" && classifierOutcome == "success") }
    public var title: String { subject.isEmpty ? "Email classification" : subject }
    public var reasonLabel: String {
        switch reason {
        case "no_reply": return "No reply requested."
        case "low_confidence": return "Not confident enough to suggest a reply."
        case "explicit_reply": return "A reply was requested."
        case "missing_body": return "Message content was unavailable."
        case "missing_headers": return "Message details were incomplete."
        case "truncated": return "Message content was incomplete."
        case "timeout": return "The classifier timed out."
        case "rate_limited": return "The classifier was temporarily rate limited."
        case "binding_error", "unavailable": return "The classifier could not run."
        case "invalid_result": return "The classifier did not return a usable result."
        default: return "No additional classification details were recorded."
        }
    }
    public var outcomeLabel: String {
        if reason == "low_confidence" && classifierOutcome == "success" { return "Not suggested" }
        switch outcome {
        case "reply": return "Decision identified"
        case "no_reply": return "No reply needed"
        case "filtered": return "Filtered"
        case "unavailable": return "Classification unavailable"
        default: return "Classification result unavailable"
        }
    }
}

public struct TodoFeed: Equatable, Sendable {
    public let captures: [TodoCapture]
    public let decisions: [TodoDecision]
    public let traces: [TodoTrace]

    public init(captures: [TodoCapture], decisions: [TodoDecision], traces: [TodoTrace], filter: TodoFeedFilter) {
        self.captures = filter == .all ? captures : []
        self.decisions = filter == .ignore ? [] : decisions.filter { filter == .all || $0.status == "needs_you" }
        self.traces = filter == .actionable ? [] : traces.filter { trace in
            guard trace.outcome != "reply", trace.decisionID == nil else { return false }
            return filter != .ignore || trace.isIgnored
        }
    }
}

public struct TodoSnapshot: Equatable, Sendable {
    public let captures: [TodoCapture]
    public let decisions: [TodoDecision]
    public let traces: [TodoTrace]
    public func feed(_ filter: TodoFeedFilter) -> TodoFeed {
        TodoFeed(captures: captures, decisions: decisions, traces: traces, filter: filter)
    }
    public init(_ json: JSON) throws {
        guard case .array(let items) = json["items"], case .array(let decisions) = json["decisions"] else { throw APIError.invalidResponse }
        captures = try items.map(TodoCapture.init)
        self.decisions = try decisions.map(TodoDecision.init)
        if case .array(let entries) = json["traces"] { traces = try entries.map(TodoTrace.init) }
        else { traces = [] }
    }
}

public extension ManagedClient {
    func todoSnapshot() async throws -> TodoSnapshot {
        try TodoSnapshot(await json(path: "/v1/todo"))
    }
    func captureTodo(_ body: String, watchHint: String = "", operationID: UUID) async throws -> TodoCapture {
        let response = try await json(path: "/v1/todo", method: "POST", body: .object([
            "body": .string(body), "watch_hint": .string(watchHint), "operation_id": .string(operationID.uuidString.lowercased()),
        ]), idempotencyKey: operationID.uuidString.lowercased())
        return try TodoCapture(response["item"])
    }
    func respondToTodoDecision(_ decision: TodoDecision, choiceID: String?, text: String?, operationID: UUID) async throws {
        guard let id = decision.id.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) else { throw APIError.invalidResponse }
        _ = try await json(path: "/v1/todo/decisions/\(id)/respond", method: "POST", body: .object([
            "version": .number(Double(decision.version)),
            "choice_id": choiceID.map(JSON.string) ?? .null,
            "text": text.map(JSON.string) ?? .null,
            "operation_id": .string(operationID.uuidString.lowercased()),
        ]), idempotencyKey: operationID.uuidString.lowercased())
    }
}
