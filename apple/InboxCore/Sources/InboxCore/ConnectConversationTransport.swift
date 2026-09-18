#if canImport(Combine) && !os(Linux)
import Foundation

public enum ConnectConversationError: LocalizedError, Equatable {
    case invalidAuthorization, conversationVisibilityRequired
    public var errorDescription: String? {
        switch self {
        case .invalidAuthorization: return "Reconnect this app to Nanocodex. Its Connect authorization is invalid."
        case .conversationVisibilityRequired: return "Allow conversation history and replies when connecting this app."
        }
    }
}

/// Receipt supplied by the host's completed Connect login. The current Connect
/// contract grants one durable agent, not a server-side multi-agent project.
/// Bearer material is excluded from descriptions and is not Codable.
public struct ConnectConversationAuthorization: Sendable, CustomStringConvertible, CustomDebugStringConvertible {
    public let origin: String
    public let grantID: String
    public let appID: String
    public let appOrigin: String
    public let agentID: String
    private let token: String
    public var description: String { "ConnectConversationAuthorization(agentID: \(agentID), token: <redacted>)" }
    public var debugDescription: String { description }

    public init(origin: String, grantID: String, appID: String, appOrigin: String,
                agentID: String, bearerToken: String, capabilities: [String]) throws {
        self.origin = try AccountCredential.normalizedOrigin(origin)
        self.appOrigin = try AccountCredential.normalizedOrigin(appOrigin)
        guard grantID.range(of: #"^0x[0-9a-fA-F]{64}$"#, options: .regularExpression) != nil,
              !appID.isEmpty, appID.count <= 256,
              appID.utf8.allSatisfy({ $0 > 32 && $0 < 127 }),
              !bearerToken.isEmpty, bearerToken.utf8.allSatisfy({ $0 > 32 && $0 < 127 }) else {
            throw ConnectConversationError.invalidAuthorization
        }
        let visible = Set(capabilities)
        guard visible.contains("agent.trace.read") ||
                (visible.contains("agent.history.read") && visible.contains("agent.output.final")) else {
            throw ConnectConversationError.conversationVisibilityRequired
        }
        _ = try ManagedClient.agentPath(agentID)
        self.grantID = grantID; self.appID = appID; self.agentID = agentID; token = bearerToken
    }

    fileprivate func authorize(_ request: inout URLRequest) {
        request.setValue("Bearer " + token, forHTTPHeaderField: "Authorization")
        request.setValue(appID, forHTTPHeaderField: "x-nanocodex-app-id")
        request.setValue(appOrigin, forHTTPHeaderField: "Origin")
    }
}

/// Real Connect transport. Every request is pinned to the grant's single agent;
/// the server checks the live grant and its output/history capabilities. The
/// grant ID namespaces component state until server project grants exist.
@MainActor public final class ConnectConversationTransport: ProjectConversationTransport {
    public let scope: ProjectConversationScope
    private let authorization: ConnectConversationAuthorization
    private let session: URLSession

    public init(authorization: ConnectConversationAuthorization, title: String,
                configuration: URLSessionConfiguration? = nil) {
        self.authorization = authorization
        scope = .init(projectID: authorization.grantID,
                      conversations: [.init(id: authorization.agentID, title: title)])
        let config = (configuration ?? .ephemeral).copy() as! URLSessionConfiguration
        config.httpAdditionalHeaders = nil; config.urlCredentialStorage = nil
        config.httpShouldSetCookies = false; config.httpCookieStorage = nil
        config.urlCache = nil; config.requestCachePolicy = .reloadIgnoringLocalCacheData
        config.timeoutIntervalForRequest = 45
        // Reopen even a heartbeat-only stream periodically so the live grant is
        // rechecked. The store resumes from delivered cursors without resending.
        config.timeoutIntervalForResource = 60
        session = URLSession(configuration: config, delegate: NoRedirects(), delegateQueue: nil)
    }
    deinit { session.invalidateAndCancel() }
    public func close() { session.invalidateAndCancel() }

    /// Internal for contract tests; callers use the typed methods below.
    func request(agentID: String, suffix: String = "", method: String = "GET",
                 body: JSON? = nil, key: String? = nil) throws -> URLRequest {
        guard agentID == authorization.agentID else { throw APIError.http(403) }
        let path = "/v1/grants/" + authorization.grantID + (try ManagedClient.agentPath(agentID)).dropFirst(3) + suffix
        guard let url = URL(string: authorization.origin + path) else { throw APIError.invalidResponse }
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        authorization.authorize(&request)
        if let body {
            let encoder = JSONEncoder(); encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
            request.httpBody = try encoder.encode(body)
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        if let key { request.setValue(key, forHTTPHeaderField: "Idempotency-Key") }
        return request
    }

    nonisolated private func json(_ request: URLRequest) async throws -> JSON {
        let (data, response) = try await session.data(for: request)
        guard let response = response as? HTTPURLResponse else { throw APIError.invalidResponse }
        guard (200..<300).contains(response.statusCode) else { throw APIError.http(response.statusCode) }
        return data.isEmpty ? .null : try JSONDecoder().decode(JSON.self, from: data)
    }

    public func state(_ id: String) async throws -> JSON { try await json(request(agentID: id)) }

    public func history(_ id: String, before: Cursor?, after: Cursor?) async throws -> EventPage {
        guard before == nil || after == nil else { throw APIError.invalidResponse }
        let suffix = "/events/history?limit=128"
            + (before.map { "&before=" + $0.rawValue } ?? "")
            + (after.map { "&after=" + $0.rawValue } ?? "")
        return try EventPage(await json(request(agentID: id, suffix: suffix)))
    }

    public func send(_ command: AgentCommand) async throws {
        guard command.kind == .followUp || command.kind == .stop,
              command.images.isEmpty, command.rawInput == nil else { throw APIError.invalidResponse }
        let spec = try command.requestSpec()
        let base = try ManagedClient.agentPath(command.agentID)
        let receipt = try await json(request(agentID: command.agentID, suffix: String(spec.path.dropFirst(base.count)),
                                   method: "POST", body: spec.body, key: spec.key))
        let expected = command.kind == .followUp ? command.requestID : command.turnID
        guard receipt["turn_id"].string == expected else { throw APIError.invalidResponse }
    }

    nonisolated public func stream(_ id: String, after: Cursor,
                       receive: @escaping @Sendable (ProjectConversationFrame) async -> Void) async throws {
        var request = try await request(agentID: id, suffix: "/events?cursor=" + after.rawValue)
        request.setValue("text/event-stream", forHTTPHeaderField: "Accept")
        let (bytes, response) = try await session.bytes(for: request)
        let task = bytes.task
        defer { task.cancel() }
        guard let response = response as? HTTPURLResponse else { throw APIError.invalidResponse }
        guard response.statusCode == 200 else { throw APIError.http(response.statusCode) }
        guard response.mimeType == "text/event-stream" else { throw APIError.invalidResponse }
        try await withTaskCancellationHandler {
            var parser = SSEParser()
            for try await byte in bytes {
                try Task.checkCancellation()
                if let frame = try parser.append(byte: byte) {
                    await receive(.init(event: frame.event, cursor: frame.cursor))
                }
            }
        } onCancel: { task.cancel() }
    }
}
#endif
