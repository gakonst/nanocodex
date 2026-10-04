import Foundation
import CryptoKit
import os

private let historyPerformanceLog = OSLog(subsystem: "xyz.paradigm.centaur", category: "Performance")
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif

public enum APIError: LocalizedError, Equatable {
    case invalidOrigin, invalidCredential, invalidResponse, agentDeleting, steeringTargetFinished, http(Int)
    public var errorDescription: String? {
        switch self {
        case .invalidOrigin: return "Enter an HTTPS server origin, without a path or query."
        case .invalidCredential: return "Enter a Nanocodex account API key."
        case .invalidResponse: return "Nanocodex returned an unreadable response. Refresh to reconnect."
        case .steeringTargetFinished: return "The active turn finished before this message arrived."
        case .agentDeleting: return "This conversation is being deleted."
        case .http(401), .http(403): return "This connection is no longer authorized. Reconnect your account."
        case .http(409): return "This turn changed before the action arrived. Refresh and try again."
        case .http(429): return "Too many requests. Wait a moment and try again."
        case .http(let code): return "The server could not complete this request (\(code))."
        }
    }
}

/// Versioned rolling recap of finalized meeting Speech segments. This is a
/// preview, not an agent turn; the final transcript is submitted only on Stop.
public struct MeetingPreview: Equatable, Sendable {
    public let revision: Int
    public let summary: String
    public let summaryRevision: Int
    public let status: String
    init(_ json: JSON, captureID: UUID) throws {
        guard json["capture_id"].string.lowercased() == captureID.uuidString.lowercased(),
              let revision = Int(exactly: json["revision"].number), revision >= 0,
              let summaryRevision = Int(exactly: json["summary_revision"].number),
              summaryRevision >= 0, summaryRevision <= revision,
              case .string(let summary) = json["summary"], summary.utf8.count <= 4096,
              ["updated", "pending", "unavailable", "unchanged"].contains(json["status"].string)
        else { throw APIError.invalidResponse }
        self.revision = revision
        self.summary = summary
        self.summaryRevision = summaryRevision
        self.status = json["status"].string
    }
}

public struct AccountCredential: Codable, Equatable, Sendable {
    public let origin: String
    public let apiKey: String
    public init(origin: String, apiKey: String) throws {
        self.origin = try Self.normalizedOrigin(origin)
        guard apiKey.range(of: #"^ncx_live_[A-Za-z0-9_-]{12}_[A-Za-z0-9_-]{43}$"#, options: .regularExpression) != nil else { throw APIError.invalidCredential }
        self.apiKey = apiKey
    }
    static func normalizedOrigin(_ origin: String) throws -> String {
        guard let url = URL(string: origin), url.scheme == "https", url.host != nil,
              url.user == nil, url.password == nil, url.query == nil, url.fragment == nil,
              url.path.isEmpty || url.path == "/" else { throw APIError.invalidOrigin }
        return origin.hasSuffix("/") ? String(origin.dropLast()) : origin
    }
}

final class NoRedirects: NSObject, URLSessionTaskDelegate, @unchecked Sendable {
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(nil)
    }
}

/// A download task writes into URLSession's temporary file, not memory. Stop it
/// during transfer rather than waiting for the entire response to reach disk.
final class BoundedOutputDownload: NSObject, URLSessionDownloadDelegate, @unchecked Sendable {
    let maximumBytes: Int64
    private let lock = NSLock()
    private var exceeded = false
    init(maximumBytes: Int64) { self.maximumBytes = maximumBytes }
    var sizeExceeded: Bool { lock.lock(); defer { lock.unlock() }; return exceeded }

    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(nil) // Task-specific delegates must preserve NoRedirects.
    }
    func urlSession(_ session: URLSession, downloadTask: URLSessionDownloadTask,
                    didWriteData bytesWritten: Int64, totalBytesWritten: Int64, totalBytesExpectedToWrite: Int64) {
        if totalBytesWritten > maximumBytes || totalBytesExpectedToWrite > maximumBytes {
            lock.lock(); exceeded = true; lock.unlock()
            downloadTask.cancel()
        }
    }
    func urlSession(_ session: URLSession, downloadTask: URLSessionDownloadTask, didFinishDownloadingTo location: URL) {}
}

/// Foundation owns HTTP freshness, validators, disk eviction and cache I/O.
/// Account-separated stores prevent credentials sharing a history cache, even
/// when callers create short-lived clients for Shortcuts or reconnects.
enum ManagedResponseCache {
    private static let lock = NSLock()
    private static let stores: NSCache<NSString, URLCache> = {
        let stores = NSCache<NSString, URLCache>()
        stores.countLimit = 4
        return stores
    }()
    static func cache(for credential: AccountCredential) -> URLCache {
        let digest = SHA256.hash(data: Data((credential.origin + "\n" + credential.apiKey).utf8))
        let key = digest.map { String(format: "%02x", $0) }.joined()
        lock.lock(); defer { lock.unlock() }
        if let cache = stores.object(forKey: key as NSString) { return cache }
        let directory = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask).first!
            .appendingPathComponent("NanocodexHistory", isDirectory: true)
            .appendingPathComponent(key, isDirectory: true)
        let cache = URLCache(memoryCapacity: 8 * 1024 * 1024,
                             diskCapacity: 128 * 1024 * 1024, directory: directory)
        stores.setObject(cache, forKey: key as NSString)
        return cache
    }
}

/// Native HTTP/SSE adapter for the existing /v1/agents contract. No embedded
/// runtime, model credentials, or execution environment is owned by this client.
public final class ManagedClient: @unchecked Sendable {
    static let maximumOutputDownloadSize: Int64 = 256 * 1024 * 1024
    let credential: AccountCredential
    let session: URLSession
    private let modelConnectionSession: URLSession
    private let responseCache: URLCache?
    private let snapshots: PersistentReadCache
    private let snapshotLifetimeLock = NSLock()
    private var snapshotWritesRetired = false
    private func snapshotTicket() -> UInt64? {
        snapshotLifetimeLock.lock(); defer { snapshotLifetimeLock.unlock() }
        return snapshotWritesRetired ? nil : snapshots.ticket()
    }
    private func retireSnapshots(clear: Bool) {
        snapshotLifetimeLock.lock(); defer { snapshotLifetimeLock.unlock() }
        snapshotWritesRetired = true
        if clear { snapshots.clear() }
    }
    private let requestOrigin: [String: String]
    private let locationContext: (@Sendable () async -> JSON?)?
    public init(credential: AccountCredential, configuration: URLSessionConfiguration? = nil, locationContext: (@Sendable () async -> JSON?)? = nil) {
        self.credential = credential
        snapshots = PersistentReadCache.scoped(to: credential)
        self.locationContext = locationContext
        #if os(iOS)
        let clientName = "ios"
        #elseif os(macOS)
        let clientName = "macos"
        #else
        let clientName = "apple"
        #endif
        requestOrigin = ["client": clientName, "timezone": TimeZone.current.identifier]
        let config = configuration ?? URLSessionConfiguration.default
        config.httpShouldSetCookies = false
        config.httpCookieStorage = nil
        if configuration == nil { config.urlCache = ManagedResponseCache.cache(for: credential) }
        responseCache = config.urlCache
        config.timeoutIntervalForRequest = 45
        config.timeoutIntervalForResource = 3600
        session = URLSession(configuration: config, delegate: NoRedirects(), delegateQueue: nil)
        // Model grants and private OAuth destinations must not persist in the HTTP cache.
        let privateConfig = (configuration?.copy() as? URLSessionConfiguration) ?? URLSessionConfiguration.ephemeral
        privateConfig.httpShouldSetCookies = false
        privateConfig.httpCookieStorage = nil
        privateConfig.urlCache = nil
        privateConfig.requestCachePolicy = .reloadIgnoringLocalCacheData
        modelConnectionSession = URLSession(configuration: privateConfig, delegate: NoRedirects(), delegateQueue: nil)
    }
    public func close() { retireSnapshots(clear: false); session.invalidateAndCancel(); modelConnectionSession.invalidateAndCancel() }
    /// Call on explicit sign-out, not when suspending an observer.
    public func clearCachedResponses() { retireSnapshots(clear: true); responseCache?.removeAllCachedResponses(); ManagedAccess.clear() }
    public func request(path: String, method: String = "GET", body: JSON? = nil, idempotencyKey: String? = nil, location: JSON? = nil) throws -> URLRequest {
        // Query values are data: a perfectly valid output filename may contain "..".
        // Keep the traversal guard on the endpoint path, not the encoded query.
        let endpoint = path.split(separator: "?", maxSplits: 1, omittingEmptySubsequences: false)[0]
        guard endpoint.hasPrefix("/v1/"), !endpoint.contains(".."), !path.contains("#"),
              let url = URL(string: credential.origin + path) else { throw APIError.invalidResponse }
        var request = URLRequest(url: url, timeoutInterval: 20)
        // Spotify's shared registration may ask the broker to wait for quota
        // before its identity read. Tokens remain inside that broker exchange.
        if path == "/v1/connectors/spotify/loopback/callback" || path == "/v1/connectors/soundcloud/loopback/callback" { request.timeoutInterval = 90 }
        // Presentation snapshots are separate. Approval/status reads must reach
        // the server even when URLCache has a previously ready draft or decision.
        if method == "GET", path.hasPrefix("/v1/todo/decisions/") || path.hasPrefix("/v1/todo/mail/drafts/") {
            request.cachePolicy = .reloadIgnoringLocalCacheData
        }
        // Full-source meeting summaries may make bounded rolling inference
        // requests for up to 90 seconds. Do not time out midway and misreport
        // a safely persisted document as lost.
        if path.hasPrefix("/v1/meetings/"), path.hasSuffix("/summarize"), method == "POST" { request.timeoutInterval = 120 }
        request.httpMethod = method
        if path == "/v1/models" || path == "/v1/credentials" || path.hasPrefix("/v1/credentials/claude") {
            request.cachePolicy = .reloadIgnoringLocalCacheData
            request.setValue("no-store", forHTTPHeaderField: "Cache-Control")
        }
        request.setValue("Bearer " + credential.apiKey, forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        if path == "/v1/agents" || path.hasPrefix("/v1/agents/") {
            var origin = requestOrigin.mapValues(JSON.string)
            if method == "POST", path == "/v1/agents" || path.hasSuffix("/turns"), let location {
                origin["location"] = location
            }
            let encoder = JSONEncoder(); encoder.outputFormatting = [.sortedKeys]
            let context = try encoder.encode(JSON.object(origin))
            request.setValue(String(decoding: context, as: UTF8.self), forHTTPHeaderField: "x-nanocodex-client-context")
        }
        if let body {
            let encoder = JSONEncoder(); encoder.outputFormatting = [.withoutEscapingSlashes]
            request.httpBody = try encoder.encode(body)
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        if let idempotencyKey { request.setValue(idempotencyKey, forHTTPHeaderField: "Idempotency-Key") }
        return request
    }
    /// A local snapshot for immediate presentation, possibly stale. This never
    /// performs network I/O; callers must refresh before relying on live state.
    public func cachedJSON(path: String) async -> JSON? {
        await Task.detached(priority: .userInitiated) { self.cachedJSONSnapshot(path: path) }.value
    }
    private func cachedJSONSnapshot(path: String) -> JSON? {
        guard PersistentReadCache.allows(path),
              let data = snapshots.read(path: path) else { return nil }
        return try? JSONDecoder().decode(JSON.self, from: data)
    }
    public func json(path: String, method: String = "GET", body: JSON? = nil, idempotencyKey: String? = nil, ifMatch: Int? = nil) async throws -> JSON {
        let snapshotTicket = snapshotTicket()
        let isAdmission = method == "POST" && (path == "/v1/agents" || (path.hasPrefix("/v1/agents/") && path.hasSuffix("/turns")))
        let location = isAdmission ? await locationContext?() : nil
        try Task.checkCancellation()
        let isHistory = path.contains("/events/history?")
        let signpostID = OSSignpostID(log: historyPerformanceLog)
        let data: Data
        let response: URLResponse
        do {
            if isHistory { os_signpost(.begin, log: historyPerformanceLog, name: "HistoryTransport", signpostID: signpostID) }
            defer { if isHistory { os_signpost(.end, log: historyPerformanceLog, name: "HistoryTransport", signpostID: signpostID) } }
            var outgoing = try request(path: path, method: method, body: body, idempotencyKey: idempotencyKey, location: location)
            if let ifMatch {
                guard ifMatch >= 0 else { throw APIError.invalidResponse }
                outgoing.setValue("\"\(ifMatch)\"", forHTTPHeaderField: "If-Match")
            }
            (data, response) = try await ManagedAccess.data(for: outgoing, using: path == "/v1/models" || path == "/v1/credentials" || path.hasPrefix("/v1/credentials/claude") ? modelConnectionSession : session)
        }
        guard let response = response as? HTTPURLResponse else { throw APIError.invalidResponse }
        guard (200..<300).contains(response.statusCode) else {
            if response.statusCode == 409, data.count <= 64 * 1024,
               (try? JSONDecoder().decode(JSON.self, from: data)["error"].string) == "agent_deleting" {
                throw APIError.agentDeleting
            }
            if response.statusCode == 409, path.hasSuffix("/steer"), data.count <= 64 * 1024,
               let rejection = try? JSONDecoder().decode(JSON.self, from: data),
               rejection["error"].string == "turn_not_steerable",
               ["completed", "failed", "cancelled"].contains(rejection["state"].string) {
                throw APIError.steeringTargetFinished
            }
            throw APIError.http(response.statusCode)
        }
        if isHistory { os_signpost(.begin, log: historyPerformanceLog, name: "HistoryJSONDecode", signpostID: signpostID, "bytes=%d", data.count) }
        defer { if isHistory { os_signpost(.end, log: historyPerformanceLog, name: "HistoryJSONDecode", signpostID: signpostID) } }
        let decoded = data.isEmpty ? JSON.null : try JSONDecoder().decode(JSON.self, from: data)
        if method != "GET", method != "HEAD" {
            let store = snapshots
            await Task.detached(priority: .utility) {
                if let snapshotTicket,
                   store.applySessionDoneMutation(path: path, method: method, response: decoded, ticket: snapshotTicket, expectedDone: body?["done"].bool) { return }
                if let snapshotTicket,
                   store.applyTodoMutation(path: path, method: method, response: decoded, ticket: snapshotTicket) { return }
                // A retired client must not recreate a Todo projection.
                if snapshotTicket == nil, path == "/v1/todo" || path.hasPrefix("/v1/todo/") { return }
                store.invalidate(path: path, method: method)
            }.value
        }
        if method == "GET", body == nil, let snapshotTicket {
            try Task.checkCancellation()
            let store = snapshots
            await Task.detached(priority: .utility) { store.save(data, path: path, ticket: snapshotTicket) }.value
        }
        return decoded
    }
    /// Private auth responses never become snapshots, arbitrary diagnostics, or access-token retries.
    /// Only a fixed status error escapes this boundary; decoder/network details may reflect secrets.
    func claudeConnectionJSON(path: String, method: String = "GET", body: JSON? = nil) async throws -> JSON {
        let allowed = ["/v1/credentials", "/v1/credentials/claude/login", "/v1/credentials/claude/login/complete", "/v1/credentials/claude"]
        guard allowed.contains(path) else { throw APIError.invalidResponse }
        do {
            try Task.checkCancellation()
            let (data, response) = try await modelConnectionSession.data(for: request(path: path, method: method, body: body))
            guard let response = response as? HTTPURLResponse else { throw APIError.invalidResponse }
            guard (200..<300).contains(response.statusCode) else { throw APIError.http(response.statusCode) }
            guard !data.isEmpty, data.count <= 64 * 1024 else { throw APIError.invalidResponse }
            return try JSONDecoder().decode(JSON.self, from: data)
        } catch is CancellationError {
            throw CancellationError()
        } catch let safe as APIError {
            throw safe
        } catch {
            throw APIError.invalidResponse
        }
    }

    public func cachedList() async -> [AgentCard]? {
        guard let body = await cachedJSON(path: "/v1/agents") else { return nil }
        return try? Self.agentCards(body)
    }
    public func list() async throws -> [AgentCard] {
        try Self.agentCards(await json(path: "/v1/agents"))
    }
    private static func agentCards(_ body: JSON) throws -> [AgentCard] {
        guard case .array(let ids) = body["data"] else { throw APIError.invalidResponse }
        return try ids.map { value in
            let id = value.string
            _ = try Self.agentPath(id)
            let summary = body["summaries"][id]
            let count = summary["turn_count"].number
            guard count >= 0, count < Double(Int.max), count.rounded(.down) == count else { throw APIError.invalidResponse }
            var card = AgentCard(id: id, title: summary["title"].string.isEmpty ? "Untitled agent" : summary["title"].string,
                             updatedAt: summary["updated_at"].number, turnCount: Int(summary["turn_count"].number),
                             mayHaveScheduledJobs: summary["may_have_scheduled_jobs"] != .bool(false),
                             lastUserMessageAt: summary["last_user_message_at"] == .null ? (count > 0 ? summary["updated_at"].number : 0) : summary["last_user_message_at"].number)
            card.applyPresentation(summary["presentation"])
            return card
        }
    }
    /// Incremental finalized text, never raw PCM or unstable Speech partials.
    /// Reuse the same capture ID and revision if a network result is uncertain.
    public func updateMeetingPreview(captureID: UUID, revision: Int, delta: String) async throws -> MeetingPreview {
        guard revision > 0, revision <= 1024, !delta.isEmpty, delta.utf8.count <= 4096 else { throw APIError.invalidResponse }
        let result = try await json(path: "/v1/meetings/" + captureID.uuidString.lowercased() + "/preview",
            method: "POST", body: .object(["revision": .number(Double(revision)), "delta": .string(delta)]))
        let preview = try MeetingPreview(result, captureID: captureID)
        guard preview.revision >= revision else { throw APIError.invalidResponse }
        return preview
    }
    public func meetingPreview(captureID: UUID) async throws -> MeetingPreview {
        try MeetingPreview(await json(path: "/v1/meetings/" + captureID.uuidString.lowercased() + "/preview"), captureID: captureID)
    }
    public func closeMeetingPreview(captureID: UUID) async throws {
        _ = try await json(path: "/v1/meetings/" + captureID.uuidString.lowercased() + "/preview", method: "DELETE")
    }
    public static func agentPath(_ id: String) throws -> String {
        guard !id.isEmpty, id.count <= 128, id.utf8.allSatisfy({ (48...57).contains($0) || (65...90).contains($0) || (97...122).contains($0) || [45, 95].contains($0) }) else { throw APIError.invalidResponse }
        return "/v1/agents/" + id
    }
    /// One explicit write, with no automatic retry on an uncertain result.
    public func setDone(_ id: String, done: Bool) async throws -> (done: Bool, doneAt: Double?, presentationRevision: Double?) {
        let result = try await json(path: Self.agentPath(id) + "/done", method: "PUT",
                                    body: .object(["done": .bool(done)]))
        guard case .bool(let confirmed) = result["done"], confirmed == done else { throw APIError.invalidResponse }
        let timestamp: Double?
        switch result["done_at"] {
        case .null: timestamp = nil
        case .number(let value) where value.isFinite && value >= 0: timestamp = value
        default: throw APIError.invalidResponse
        }
        guard confirmed ? timestamp != nil : timestamp == nil else { throw APIError.invalidResponse }
        let revision: Double?
        switch result["presentation_revision"] {
        case .null: revision = nil // Older services did not return ordering metadata.
        case .number(let value) where value.isFinite && value >= 0 && value.rounded(.down) == value: revision = value
        default: throw APIError.invalidResponse
        }
        return (confirmed, timestamp, revision)
    }
    public func state(_ id: String) async throws -> JSON { try await json(path: Self.agentPath(id)) }
    /// Starts server-owned preparation without waiting for model readiness.
    public func prepare(_ id: String) async throws {
        _ = try await json(path: Self.agentPath(id) + "/prepare", method: "POST")
    }
    /// Refresh every unique agent with at most four operations in flight. Resolve
    /// history policy when a slot opens, so a tab switch can change who owns it.
    public func refreshAgents(_ agentIDs: [String],
                              history: @escaping @Sendable (String) async -> AgentRefreshHistory?,
                              onResult: @escaping @Sendable (String, Result<AgentRefreshResult, Error>) async -> Void) async {
        let read: @Sendable (String) async -> (String, Result<AgentRefreshResult, Error>?) = { id in
            do {
                try Task.checkCancellation()
                guard let policy = await history(id) else { return (id, nil) }
                try Task.checkCancellation()
                switch policy {
                case .initial:
                    async let state = self.state(id)
                    async let page = self.history(id)
                    return try await (id, .success(AgentRefreshResult(state: state, page: page)))
                case .changed(let cursor):
                    let state = try await self.state(id)
                    let changed = Cursor(rawValue: state["latest_event_cursor"].string).map { $0 > cursor } ?? true
                    let page = changed ? try await self.history(id) : nil
                    return (id, .success(AgentRefreshResult(state: state, page: page)))
                case .stateOnly:
                    return try await (id, .success(AgentRefreshResult(state: self.state(id), page: nil)))
                }
            } catch { return (id, .failure(error)) }
        }
        await withTaskGroup(of: (String, Result<AgentRefreshResult, Error>?).self) { group in
            var seen = Set<String>()
            var remaining = agentIDs.filter { seen.insert($0).inserted }.makeIterator()
            for _ in 0..<4 {
                guard !Task.isCancelled, let id = remaining.next() else { break }
                group.addTask { await read(id) }
            }
            while let (id, result) = await group.next() {
                guard !Task.isCancelled else { group.cancelAll(); return }
                if let next = remaining.next() { group.addTask { await read(next) } }
                if let result { await onResult(id, result) }
            }
        }
    }
    public func cachedScheduledJobs(_ agentID: String) async -> [ScheduledJob]? {
        guard let path = try? Self.agentPath(agentID), let body = await cachedJSON(path: path + "/triggers") else { return nil }
        return try? Self.parseScheduledJobs(body, agentID: agentID)
    }
    public func scheduledJobs(_ agentID: String) async throws -> [ScheduledJob] {
        try Self.parseScheduledJobs(await json(path: Self.agentPath(agentID) + "/triggers"), agentID: agentID)
    }
    private static func parseScheduledJobs(_ body: JSON, agentID: String) throws -> [ScheduledJob] {
        guard case .array(let values) = body["data"] else { throw APIError.invalidResponse }
        let jobs = try values.map { try ScheduledJob($0, agentID: agentID) }
        guard Set(jobs.map(\.id)).count == jobs.count else { throw APIError.invalidResponse }
        return jobs
    }
    public func updateScheduledJob(_ job: ScheduledJob, cron: String, timezone: String, input: String,
                                   enabled: Bool, startsNewConversation: Bool) async throws -> ScheduledJob {
        let body: JSON = .object(["cron": .string(cron), "timezone": .string(timezone), "input": .string(input),
                                  "enabled": .bool(enabled), "session_mode": .string(startsNewConversation ? "new" : "continue")])
        let value = try await json(path: Self.agentPath(job.agentID) + "/triggers/" + job.triggerID, method: "PATCH", body: body)
        let updated = try ScheduledJob(value, agentID: job.agentID)
        guard updated.id == job.id else { throw APIError.invalidResponse }
        return updated
    }

    public func cancelScheduledJob(_ job: ScheduledJob) async throws {
        _ = try await json(path: Self.agentPath(job.agentID) + "/triggers/" + job.triggerID, method: "DELETE")
    }

    /// Deliver each agent's schedules immediately, keeping four reads in flight.
    /// A slow agent must not hold up completed results or the next agent's read.
    public func scheduledJobs(for agentIDs: [String],
                              onResult: @Sendable (String, Result<[ScheduledJob], Error>) async -> Void) async {
        let read: @Sendable (String) async -> (String, Result<[ScheduledJob], Error>) = { id in
            do { try Task.checkCancellation(); return (id, .success(try await self.scheduledJobs(id))) }
            catch { return (id, .failure(error)) }
        }
        await withTaskGroup(of: (String, Result<[ScheduledJob], Error>).self) { group in
            var seen = Set<String>()
            var missing: [String] = []
            var remaining = agentIDs.filter { seen.insert($0).inserted }.makeIterator()
            for _ in 0..<4 {
                guard !Task.isCancelled, let id = remaining.next() else { break }
                group.addTask { await read(id) }
            }
            while let (id, result) = await group.next() {
                guard !Task.isCancelled else { group.cancelAll(); return }
                if let next = remaining.next() { group.addTask { await read(next) } }
                if case .failure(let error) = result, error as? APIError == .http(404) {
                    missing.append(id)
                } else {
                    await onResult(id, result)
                }
            }
            guard !missing.isEmpty, !Task.isCancelled else { return }
            // A deletion can race the account roster. Only a fresh authenticated
            // roster may confirm that a missing owner has no schedules to show;
            // a missing route or an advertised but unreadable agent stays an error.
            do {
                let owners = Set(try await self.list().map(\.id))
                guard !Task.isCancelled else { return }
                for id in missing {
                    guard !Task.isCancelled else { return }
                    await onResult(id, owners.contains(id) ? .failure(APIError.http(404)) : .success([]))
                }
            } catch {
                guard !Task.isCancelled else { return }
                for id in missing {
                    guard !Task.isCancelled else { return }
                    await onResult(id, .failure(error))
                }
            }
        }
    }
    public func turn(agentID: String, turnID: String) async throws -> JSON {
        let cancelPath = try AgentCommand(agentID: agentID, turnID: turnID, kind: .stop).requestSpec().path
        return try await json(path: String(cancelPath.dropLast("/cancel".count)))
    }
    public func history(_ id: String, before: Cursor? = nil, after: Cursor? = nil) async throws -> EventPage {
        guard before == nil || after == nil else { throw APIError.invalidResponse }
        let path = try Self.agentPath(id) + "/events/history?limit=128"
            + (before.map { "&before=" + $0.rawValue } ?? "") + (after.map { "&after=" + $0.rawValue } ?? "")
        let body: JSON
        do {
            body = try await json(path: path)
        } catch let error as URLError where [URLError.notConnectedToInternet, .networkConnectionLost,
                                              .cannotConnectToHost, .cannotFindHost, .dnsLookupFailed,
                                              .timedOut].contains(error.code) {
            try Task.checkCancellation()
            guard let cached = await cachedJSON(path: path) else { throw error }
            body = cached
        }
        let signpostID = OSSignpostID(log: historyPerformanceLog)
        os_signpost(.begin, log: historyPerformanceLog, name: "HistoryEventPreparation", signpostID: signpostID)
        defer { os_signpost(.end, log: historyPerformanceLog, name: "HistoryEventPreparation", signpostID: signpostID) }
        return try EventPage(body)
    }
    /// Prepare the latest page so stream observation can begin without waiting
    /// for older history. Callers backfill from the first retained event's cursor.
    /// Save a contiguous streamed tail using the same durable page as an opening read.
    public func saveConversationSnapshot(_ id: String, events: [AgentEvent], latest: Cursor, hasMore: Bool) async {
        guard let path = try? Self.agentPath(id) else { return }
        guard let ticket = snapshotTicket() else { return }
        let store = snapshots
        await Task.detached(priority: .utility) {
            let values: [JSON] = events.compactMap { event in
                guard case .object(var value) = event.data else { return nil }
                value["cursor"] = .string(event.cursor.rawValue)
                return .object(value)
            }
            guard values.count == events.count else { return }
            let page = JSON.object(["data": .array(values), "latest_cursor": .string(latest.rawValue), "has_more": .bool(hasMore)])
            guard let data = try? JSONEncoder().encode(page) else { return }
            store.save(data, path: path + "/events/history?limit=128", ticket: ticket)
        }.value
    }
    public func cachedConversationHistory(_ id: String) async -> ConversationHistory? {
        guard let path = try? Self.agentPath(id),
              let body = await cachedJSON(path: path + "/events/history?limit=128"),
              let page = try? EventPage(body) else { return nil }
        return try? await Self.prepareConversation(page)
    }
    public func conversationHistory(_ id: String) async throws -> ConversationHistory {
        let signpostID = OSSignpostID(log: historyPerformanceLog)
        os_signpost(.begin, log: historyPerformanceLog, name: "HistoryOpening", signpostID: signpostID)
        defer { os_signpost(.end, log: historyPerformanceLog, name: "HistoryOpening", signpostID: signpostID) }
        try Task.checkCancellation()
        let page = try await history(id)
        return try await Self.prepareConversation(page)
    }
    private static func prepareConversation(_ page: EventPage) async throws -> ConversationHistory {
        guard !page.hasMore || !page.events.isEmpty else { throw APIError.invalidResponse }
        var events = page.events
        var counts = try await TranscriptPreparation.byteCounts(events)
        // Keep the newest edge while allowing one oversized event to remain whole.
        let removed = TranscriptRetention.removablePrefixCount(byteCounts: counts,
            retainedBytes: counts.reduce(0, +), byteLimit: 16 * 1024 * 1024)
        if removed > 0 {
            events.removeFirst(removed)
            counts.removeFirst(removed)
        }
        let projector = TranscriptStreamProjection()
        let rows = try await projector.rows(events)
        try Task.checkCancellation()
        return ConversationHistory(events: events, latest: page.latest, hasMore: page.hasMore || removed > 0,
                                   byteCounts: counts, rows: rows, hasNewer: false, projector: projector)
    }
    @discardableResult
    public func command(_ command: AgentCommand) async throws -> JSON {
        let spec = try command.requestSpec()
        return try await json(path: spec.path, method: "POST", body: spec.body, idempotencyKey: spec.key)
    }
    /// Upload bounded chunks, retaining the attachment ID across retries. The
    /// service owns multipart state; credentials and upload IDs never enter a turn.
    public func uploadAttachment(agentID: String, attachment: MessageAttachment, source: URL, preview: URL? = nil,
                            isCancelled: @Sendable () async -> Bool = { false }) async throws -> String {
        try Task.checkCancellation()
        if await isCancelled() { throw CancellationError() }
        guard source.isFileURL else { throw AttachmentError.invalidReference }
        let values = try source.resourceValues(forKeys: [.fileSizeKey, .isRegularFileKey, .isSymbolicLinkKey])
        guard values.isRegularFile == true, values.isSymbolicLink != true, values.fileSize == attachment.byteCount else { throw AttachmentError.invalidReference }
        let path = try Self.agentPath(agentID) + "/attachments/" + attachment.id.lowercased()
        let receipt = try await json(path: path, method: "POST", body: .object([
            "name": .string(attachment.name), "media_type": .string(attachment.mediaType), "size": .number(Double(attachment.byteCount))]))
        let filePath = receipt["path"].string
        _ = try attachment.originalContent(path: filePath)
        guard receipt["size"].number == Double(attachment.byteCount) else { throw APIError.invalidResponse }
        if receipt["complete"] == .bool(true) {
            try Task.checkCancellation()
            if await isCancelled() { throw CancellationError() }
            if let preview { try await uploadPreview(path: path, source: preview) }
            return filePath
        }
        guard let partSize = Int(exactly: receipt["part_size"].number), partSize > 0 else { throw APIError.invalidResponse }
        let number = receipt["next_part"].number
        let count = (attachment.byteCount - 1) / partSize + 1
        guard number >= 1, number <= Double(count + 1), number.rounded(.down) == number else { throw APIError.invalidResponse }
        let file = try FileHandle(forReadingFrom: source)
        defer { try? file.close() }
        var part = Int(number)
        try file.seek(toOffset: UInt64(min(attachment.byteCount, (part - 1) * partSize)))
        while part <= count {
            try Task.checkCancellation()
            if await isCancelled() { throw CancellationError() }
            let expected = min(partSize, attachment.byteCount - (part - 1) * partSize)
            let temporary = FileManager.default.temporaryDirectory.appendingPathComponent("attachment-part-" + UUID().uuidString)
            guard FileManager.default.createFile(atPath: temporary.path, contents: nil) else { throw AttachmentError.unavailable }
            defer { try? FileManager.default.removeItem(at: temporary) }
            let output = try FileHandle(forWritingTo: temporary)
            defer { try? output.close() }
            var remaining = expected
            while remaining > 0 {
                try Task.checkCancellation()
                guard let chunk = try file.read(upToCount: min(8 * 1024 * 1024, remaining)), !chunk.isEmpty else { throw AttachmentError.unavailable }
                try output.write(contentsOf: chunk)
                remaining -= chunk.count
            }
            try output.close()
            var request = try request(path: path + "/parts/" + String(part), method: "PUT")
            request.timeoutInterval = 120
            request.setValue("application/octet-stream", forHTTPHeaderField: "Content-Type")
            let (_, response) = try await session.upload(for: request, fromFile: temporary)
            guard let response = response as? HTTPURLResponse else { throw APIError.invalidResponse }
            guard response.statusCode == 200 else { throw APIError.http(response.statusCode) }
            part += 1
        }
        try Task.checkCancellation()
        if await isCancelled() { throw CancellationError() }
        let complete = try await json(path: path + "/complete", method: "POST")
        guard complete["complete"] == .bool(true), complete["path"].string == filePath,
              complete["size"].number == Double(attachment.byteCount) else { throw APIError.invalidResponse }
        try Task.checkCancellation()
        if await isCancelled() { throw CancellationError() }
        if let preview { try await uploadPreview(path: path, source: preview) }
        return filePath
    }

    private func uploadPreview(path: String, source: URL) async throws {
        var request = try request(path: path + "/preview", method: "PUT")
        request.setValue("image/jpeg", forHTTPHeaderField: "Content-Type")
        let (_, response) = try await session.upload(for: request, fromFile: source)
        guard let response = response as? HTTPURLResponse else { throw APIError.invalidResponse }
        guard response.statusCode == 200 else { throw APIError.http(response.statusCode) }
    }

    /// Download a private /brain/outputs link with this account's credentials.
    /// The sandbox URI itself is never passed to URLSession or an external app.
    public func downloadOutput(agentID: String, path: String) async throws -> URL {
        guard PublishedOutputLink.validPath(path) else { throw APIError.invalidResponse }
        var query = URLComponents()
        query.queryItems = [URLQueryItem(name: "path", value: path)]
        guard let encoded = query.percentEncodedQuery else { throw APIError.invalidResponse }
        var request = try request(path: Self.agentPath(agentID) + "/files?" + encoded)
        request.timeoutInterval = 300
        request.setValue("application/octet-stream", forHTTPHeaderField: "Accept")
        let limiter = BoundedOutputDownload(maximumBytes: Self.maximumOutputDownloadSize)
        let download: URL
        let response: URLResponse
        do { (download, response) = try await session.download(for: request, delegate: limiter) }
        catch { if limiter.sizeExceeded { throw APIError.invalidResponse }; throw error }
        defer { try? FileManager.default.removeItem(at: download) }
        guard !limiter.sizeExceeded else { throw APIError.invalidResponse }
        guard let response = response as? HTTPURLResponse else { throw APIError.invalidResponse }
        guard response.statusCode == 200 else { throw APIError.http(response.statusCode) }
        guard response.mimeType == "application/octet-stream" else { throw APIError.invalidResponse }
        let size = try download.resourceValues(forKeys: [.fileSizeKey]).fileSize
        guard let size, size >= 0, Int64(size) <= Self.maximumOutputDownloadSize,
              response.expectedContentLength <= Self.maximumOutputDownloadSize,
              response.expectedContentLength < 0 || Int64(size) == response.expectedContentLength else {
            throw APIError.invalidResponse
        }
        // An isolated folder keeps the original filename in Quick Look and
        // Save to Files without colliding with another download of that name.
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("NanocodexOutput-" + UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let local = directory.appendingPathComponent((path as NSString).lastPathComponent)
        do { try FileManager.default.moveItem(at: download, to: local) }
        catch { try? FileManager.default.removeItem(at: directory); throw error }
        if Task.isCancelled { try? FileManager.default.removeItem(at: directory); throw CancellationError() }
        return local
    }

    /// Account-scoped URLCache retains immutable previews; original bytes never
    /// enter a scrolling transcript or the in-memory history projection.
    public func attachmentPreview(agentID: String, attachmentID: String) async throws -> Data {
        guard MessageAttachment.validID(attachmentID) else { throw AttachmentError.invalidReference }
        let request = try request(path: Self.agentPath(agentID) + "/attachments/" + attachmentID.lowercased() + "/preview", method: "GET")
        let (data, response) = try await session.data(for: request)
        guard let response = response as? HTTPURLResponse else { throw APIError.invalidResponse }
        guard response.statusCode == 200 else { throw APIError.http(response.statusCode) }
        guard response.mimeType == "image/jpeg" else { throw APIError.invalidResponse }
        return data
    }

    /// The native previewer receives a local file, never an account credential or bearer URL.
    public func downloadAttachment(agentID: String, attachment: MessageAttachment) async throws -> URL {
        var request = try request(path: Self.agentPath(agentID) + "/attachments/" + attachment.id.lowercased())
        request.timeoutInterval = 120
        request.setValue(attachment.mediaType, forHTTPHeaderField: "Accept")
        let (download, response) = try await session.download(for: request)
        defer { try? FileManager.default.removeItem(at: download) }
        guard let response = response as? HTTPURLResponse else { throw APIError.invalidResponse }
        guard response.statusCode == 200 else { throw APIError.http(response.statusCode) }
        guard try download.resourceValues(forKeys: [.fileSizeKey]).fileSize == attachment.byteCount else { throw APIError.invalidResponse }
        let suffix = URL(fileURLWithPath: attachment.originalPath).pathExtension
        let local = FileManager.default.temporaryDirectory.appendingPathComponent("attachment-" + UUID().uuidString).appendingPathExtension(suffix)
        try FileManager.default.moveItem(at: download, to: local)
        if Task.isCancelled { try? FileManager.default.removeItem(at: local); throw CancellationError() }
        return local
    }

    /// AVPlayer receives a local file, never an account credential or bearer URL.
    /// The caller removes this temporary copy when playback closes.
    public func downloadVideo(agentID: String, video: TranscriptVideo) async throws -> URL {
        guard MessageAttachment.validID(video.id), let mediaType = video.mediaType,
              ["video/mp4", "video/quicktime"].contains(mediaType), let size = video.byteCount,
              video.path == VideoAttachmentContent.path(id: video.id, mediaType: mediaType) else { throw AttachmentError.invalidReference }
        var request = try request(path: Self.agentPath(agentID) + "/attachments/" + video.id.lowercased())
        request.timeoutInterval = 120
        request.setValue(mediaType, forHTTPHeaderField: "Accept")
        let (download, response) = try await session.download(for: request)
        defer { try? FileManager.default.removeItem(at: download) }
        guard let response = response as? HTTPURLResponse else { throw APIError.invalidResponse }
        guard response.statusCode == 200 else { throw APIError.http(response.statusCode) }
        guard try download.resourceValues(forKeys: [.fileSizeKey]).fileSize == size else { throw APIError.invalidResponse }
        let local = FileManager.default.temporaryDirectory.appendingPathComponent("video-playback-" + UUID().uuidString + (mediaType == "video/quicktime" ? ".mov" : ".mp4"))
        try FileManager.default.moveItem(at: download, to: local)
        return local
    }
    public func create(requestID: String, codeModeAsync: Bool = false) async throws -> String {
        // The managed contract uses an absent body for default settings; {} is invalid.
        let configuration: JSON? = codeModeAsync ? .object(["configuration": .object(["code_mode_async": .bool(true)])]) : nil
        let body = try await json(path: "/v1/agents", method: "POST", body: configuration, idempotencyKey: requestID)
        let id = body["agent_id"].string
        _ = try Self.agentPath(id)
        return id
    }
    #if !os(Linux)
    public func stream(_ id: String, after cursor: Cursor,
                       onOpen: (@Sendable () async -> Void)? = nil,
                       receive: @escaping @Sendable (SSEFrame) async -> Void) async throws {
        try await stream(id, after: cursor, idleCheckInterval: .seconds(15), onOpen: onOpen, receive: receive)
    }

    func stream(_ id: String, after cursor: Cursor, idleCheckInterval: Duration,
                onOpen: (@Sendable () async -> Void)? = nil,
                receive: @escaping @Sendable (SSEFrame) async -> Void) async throws {
        var request = try request(path: Self.agentPath(id) + "/events?cursor=" + cursor.rawValue)
        request.timeoutInterval = 45
        request.setValue("text/event-stream", forHTTPHeaderField: "Accept")
        let (bytes, response) = try await session.bytes(for: request)
        let task = bytes.task
        defer { task.cancel() }
        guard let response = response as? HTTPURLResponse else { throw APIError.invalidResponse }
        guard response.statusCode == 200 else { throw APIError.http(response.statusCode) }
        guard response.mimeType == "text/event-stream" else { throw APIError.invalidResponse }
        await onOpen?()
        let progress = StreamDeliveryProgress(cursor: cursor)
        // Keepalives prove the connection is open, not that durable events are
        // reaching it. An idle stream behind durable state must replay from its
        // consumer's cursor; the snapshot cursor is never delivered or adopted.
        try await withThrowingTaskGroup(of: Void.self) { group in
            group.addTask {
                try await withTaskCancellationHandler {
                    var parser = SSEParser()
                    for try await byte in bytes {
                        try Task.checkCancellation()
                        if let frame = try parser.append(byte: byte) {
                            await progress.beginDelivery(frame)
                            await receive(frame)
                            await progress.finishDelivery(frame)
                        }
                    }
                } onCancel: { task.cancel() }
            }
            group.addTask {
                while !Task.isCancelled {
                    try await Task.sleep(for: idleCheckInterval)
                    guard let delivered = await progress.idleCursor(for: idleCheckInterval) else { continue }
                    let state: JSON
                    do { state = try await self.state(id) }
                    catch {
                        try Task.checkCancellation()
                        continue // An unavailable snapshot does not invalidate a live stream.
                    }
                    guard state["agent_id"].string == id,
                          let latest = Cursor(rawValue: state["latest_event_cursor"].string),
                          await progress.isStillBehind(latest, since: delivered) else { continue }
                    throw URLError(.networkConnectionLost)
                }
                throw CancellationError()
            }
            defer { group.cancelAll(); task.cancel() }
            try await group.next()
        }
    }
    #endif
}

#if !os(Linux)
private actor StreamDeliveryProgress {
    private var cursor: Cursor
    private var advancedAt = ContinuousClock.now
    private var deliveringEvent = false
    init(cursor: Cursor) { self.cursor = cursor }
    func beginDelivery(_ frame: SSEFrame) { deliveringEvent = frame.event != nil }
    func finishDelivery(_ frame: SSEFrame) {
        if let next = frame.cursor, next > cursor {
            cursor = next
            advancedAt = .now
        }
        deliveringEvent = false
    }
    func idleCursor(for interval: Duration) -> Cursor? {
        !deliveringEvent && advancedAt.duration(to: .now) >= interval ? cursor : nil
    }
    func isStillBehind(_ latest: Cursor, since snapshot: Cursor) -> Bool {
        !deliveringEvent && cursor == snapshot && latest > cursor
    }
}
#endif

public enum AgentRefreshHistory: Sendable {
    case initial
    case changed(after: Cursor)
    case stateOnly
}

public struct AgentRefreshResult: Sendable {
    public let state: JSON
    public let page: EventPage?
}

public struct ConversationHistory: Sendable {
    public let events: [AgentEvent]
    public let latest: Cursor
    public let hasMore: Bool
    public let byteCounts: [Int]
    public let rows: [TranscriptRow]
    public let hasNewer: Bool
    /// Transfer the prepared window into its stream observer without replaying it.
    public let projector: TranscriptStreamProjection
}

public struct EventPage: Sendable {
    public let events: [AgentEvent]
    public let latest: Cursor
    public let hasMore: Bool
    public init(_ body: JSON) throws {
        guard case .array(let data) = body["data"], case .bool(let more) = body["has_more"],
              let latest = Cursor(rawValue: body["latest_cursor"].string) else { throw APIError.invalidResponse }
        let events = try data.map { try AgentEvent($0) }
        guard zip(events, events.dropFirst()).allSatisfy({ pair in pair.0.cursor < pair.1.cursor }),
              events.last.map({ $0.cursor <= latest }) ?? true else { throw APIError.invalidResponse }
        self.events = events; self.latest = latest; hasMore = more
    }
}

/// Capture agent and turn identity at the button press, before any await or swipe.
public struct AgentCommand: Equatable, Sendable {
    public enum Kind: Equatable, Sendable { case followUp, steer, withdrawSteer, stop }
    public let agentID: String
    public let turnID: String
    public let input: String
    public var images: [JSON] = []
    public var rawInput: JSON?
    public let kind: Kind
    public let requestID: String
    public init(agentID: String, turnID: String = "", input: String = "", kind: Kind, requestID: String = UUID().uuidString) {
        self.agentID = agentID; self.turnID = turnID; self.input = input; self.kind = kind; self.requestID = requestID
    }
    public func requestSpec() throws -> (path: String, body: JSON?, key: String?) {
        let path = try ManagedClient.agentPath(agentID) + "/turns"
        let content: JSON = rawInput ?? (images.isEmpty ? .string(input) : .array(
            (input.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? [] : [.object(["type": .string("text"), "text": .string(input)])]) + images
        ))
        let body: JSON = .object(kind == .followUp ? ["id": .string(requestID), "input": content] : ["input": content, "message_id": .string(requestID)])
        if kind == .followUp { return (path, body, "inbox:" + requestID) }
        guard !turnID.isEmpty, turnID.range(of: #"^[A-Za-z0-9._:-]{1,128}$"#, options: .regularExpression) != nil,
              let segment = turnID.addingPercentEncoding(withAllowedCharacters: .alphanumerics) else { throw APIError.invalidResponse }
        if kind == .withdrawSteer { return (path + "/" + segment + "/withdraw-steer", .object(["message_id": .string(requestID)]), nil) }
        return (path + "/" + segment + (kind == .steer ? "/steer" : "/cancel"), kind == .steer ? body : nil, nil)
    }
}
