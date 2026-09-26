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

public struct TodoSnapshot: Equatable, Sendable {
    public let captures: [TodoCapture]
    public let decisions: [TodoDecision]
    public init(_ json: JSON) throws {
        guard case .array(let items) = json["items"], case .array(let decisions) = json["decisions"] else { throw APIError.invalidResponse }
        captures = try items.map(TodoCapture.init)
        self.decisions = try decisions.map(TodoDecision.init)
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
