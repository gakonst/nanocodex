import Foundation
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif

public enum APIError: LocalizedError, Equatable {
    case invalidOrigin, invalidCredential, invalidResponse, http(Int)
    public var errorDescription: String? {
        switch self {
        case .invalidOrigin: return "Enter an HTTPS server origin, without a path or query."
        case .invalidCredential: return "Enter a Nanocodex account API key."
        case .invalidResponse: return "Nanocodex returned an unreadable response. Refresh to reconnect."
        case .http(401), .http(403): return "This connection is no longer authorized. Reconnect your account."
        case .http(409): return "This turn changed before the action arrived. Refresh and try again."
        case .http(429): return "Too many requests. Wait a moment and try again."
        case .http(let code): return "The server could not complete this request (\(code))."
        }
    }
}

public struct AccountCredential: Codable, Sendable {
    public let origin: String
    public let apiKey: String
    public init(origin: String, apiKey: String) throws {
        guard let url = URL(string: origin), url.scheme == "https", url.host != nil,
              url.user == nil, url.password == nil, url.query == nil, url.fragment == nil,
              url.path.isEmpty || url.path == "/" else { throw APIError.invalidOrigin }
        guard apiKey.range(of: #"^ncx_live_[A-Za-z0-9_-]{12}_[A-Za-z0-9_-]{43}$"#, options: .regularExpression) != nil else { throw APIError.invalidCredential }
        self.origin = origin.hasSuffix("/") ? String(origin.dropLast()) : origin
        self.apiKey = apiKey
    }
}

private final class NoRedirects: NSObject, URLSessionTaskDelegate, @unchecked Sendable {
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(nil)
    }
}

/// Native HTTP/SSE adapter for the existing /v1/agents contract. No embedded
/// runtime, model credentials, or execution environment is owned by this client.
public final class ManagedClient: @unchecked Sendable {
    private let credential: AccountCredential
    private let session: URLSession
    public init(credential: AccountCredential) {
        self.credential = credential
        let config = URLSessionConfiguration.ephemeral
        config.httpShouldSetCookies = false
        config.urlCache = nil
        config.timeoutIntervalForRequest = 45
        config.timeoutIntervalForResource = 3600
        session = URLSession(configuration: config, delegate: NoRedirects(), delegateQueue: nil)
    }
    public func close() { session.invalidateAndCancel() }
    public func request(path: String, method: String = "GET", body: JSON? = nil, idempotencyKey: String? = nil) throws -> URLRequest {
        guard path.hasPrefix("/v1/"), !path.contains(".."), !path.contains("#"),
              let url = URL(string: credential.origin + path) else { throw APIError.invalidResponse }
        var request = URLRequest(url: url, timeoutInterval: 20)
        request.httpMethod = method
        request.setValue("Bearer " + credential.apiKey, forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        if let body {
            request.httpBody = try JSONEncoder().encode(body)
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        if let idempotencyKey { request.setValue(idempotencyKey, forHTTPHeaderField: "Idempotency-Key") }
        return request
    }
    public func json(path: String, method: String = "GET", body: JSON? = nil, idempotencyKey: String? = nil) async throws -> JSON {
        let (data, response) = try await session.data(for: request(path: path, method: method, body: body, idempotencyKey: idempotencyKey))
        guard let response = response as? HTTPURLResponse else { throw APIError.invalidResponse }
        guard (200..<300).contains(response.statusCode) else { throw APIError.http(response.statusCode) }
        guard data.count <= 32 * 1024 * 1024 else { throw APIError.invalidResponse }
        return data.isEmpty ? .null : try JSONDecoder().decode(JSON.self, from: data)
    }
    public func list() async throws -> [AgentCard] {
        let body = try await json(path: "/v1/agents")
        guard case .array(let ids) = body["data"] else { throw APIError.invalidResponse }
        return try ids.map { value in
            let id = value.string
            _ = try Self.agentPath(id)
            let summary = body["summaries"][id]
            let count = summary["turn_count"].number
            guard count >= 0, count < Double(Int.max), count.rounded(.down) == count else { throw APIError.invalidResponse }
            return AgentCard(id: id, title: summary["title"].string.isEmpty ? "Untitled agent" : summary["title"].string,
                             updatedAt: summary["updated_at"].number, turnCount: Int(summary["turn_count"].number))
        }
    }
    public static func agentPath(_ id: String) throws -> String {
        guard !id.isEmpty, id.count <= 128, id.utf8.allSatisfy({ (48...57).contains($0) || (65...90).contains($0) || (97...122).contains($0) || [45, 95].contains($0) }) else { throw APIError.invalidResponse }
        return "/v1/agents/" + id
    }
    public func state(_ id: String) async throws -> JSON { try await json(path: Self.agentPath(id)) }
    public func history(_ id: String, before: Cursor? = nil) async throws -> EventPage {
        let path = try Self.agentPath(id) + "/events/history?limit=128" + (before.map { "&before=" + $0.rawValue } ?? "")
        return try EventPage(try await json(path: path))
    }
    public func command(_ command: AgentCommand) async throws {
        let spec = try command.requestSpec()
        _ = try await json(path: spec.path, method: "POST", body: spec.body, idempotencyKey: spec.key)
    }
    public func create(requestID: String) async throws -> String {
        let body = try await json(path: "/v1/agents", method: "POST", body: .object([:]), idempotencyKey: requestID)
        let id = body["agent_id"].string
        _ = try Self.agentPath(id)
        return id
    }
    #if !os(Linux)
    public func stream(_ id: String, after cursor: Cursor, receive: @escaping @Sendable (SSEFrame) async -> Void) async throws {
        var request = try request(path: Self.agentPath(id) + "/events?cursor=" + cursor.rawValue)
        request.timeoutInterval = 45
        request.setValue("text/event-stream", forHTTPHeaderField: "Accept")
        let (bytes, response) = try await session.bytes(for: request)
        guard let response = response as? HTTPURLResponse else { throw APIError.invalidResponse }
        guard response.statusCode == 200 else { throw APIError.http(response.statusCode) }
        guard response.mimeType == "text/event-stream" else { throw APIError.invalidResponse }
        var parser = SSEParser()
        for try await byte in bytes {
            try Task.checkCancellation()
            if let frame = try parser.append(byte: byte) { await receive(frame) }
        }
    }
    #endif
}

public struct EventPage: Sendable {
    public let events: [AgentEvent]
    public let latest: Cursor
    public let hasMore: Bool
    public init(_ body: JSON) throws {
        guard case .array(let data) = body["data"], case .bool(let more) = body["has_more"],
              let latest = Cursor(rawValue: body["latest_cursor"].string) else { throw APIError.invalidResponse }
        let events = try data.map { try AgentEvent($0) }
        guard events.count <= 128, zip(events, events.dropFirst()).allSatisfy({ pair in pair.0.cursor < pair.1.cursor }),
              events.last.map({ $0.cursor <= latest }) ?? true else { throw APIError.invalidResponse }
        self.events = events; self.latest = latest; hasMore = more
    }
}

/// Capture agent and turn identity at the button press, before any await or swipe.
public struct AgentCommand: Equatable, Sendable {
    public enum Kind: Equatable, Sendable { case followUp, steer, stop }
    public let agentID: String
    public let turnID: String
    public let input: String
    public let kind: Kind
    public let requestID: String
    public init(agentID: String, turnID: String = "", input: String = "", kind: Kind, requestID: String = UUID().uuidString) {
        self.agentID = agentID; self.turnID = turnID; self.input = input; self.kind = kind; self.requestID = requestID
    }
    public func requestSpec() throws -> (path: String, body: JSON?, key: String?) {
        let path = try ManagedClient.agentPath(agentID) + "/turns"
        if kind == .followUp {
            return (path, .object(["id": .string(requestID), "input": .string(input)]), "inbox:" + requestID)
        }
        guard !turnID.isEmpty, turnID.range(of: #"^[A-Za-z0-9._:-]{1,128}$"#, options: .regularExpression) != nil,
              let segment = turnID.addingPercentEncoding(withAllowedCharacters: .alphanumerics) else { throw APIError.invalidResponse }
        return (path + "/" + segment + (kind == .steer ? "/steer" : "/cancel"), kind == .steer ? .object(["input": .string(input)]) : nil, nil)
    }
}
