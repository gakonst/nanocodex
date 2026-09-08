import Foundation
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif

public enum APIError: LocalizedError, Equatable {
    case invalidOrigin, invalidCredential, invalidResponse, agentDeleting, messageTooLarge, http(Int)
    public var errorDescription: String? {
        switch self {
        case .invalidOrigin: return "Enter an HTTPS server origin, without a path or query."
        case .invalidCredential: return "Enter a Nanocodex account API key."
        case .invalidResponse: return "Nanocodex returned an unreadable response. Refresh to reconnect."
        case .agentDeleting: return "This conversation is being deleted."
        case .messageTooLarge: return "This message is too large. Remove an image or shorten the message."
        case .http(401), .http(403): return "This connection is no longer authorized. Reconnect your account."
        case .http(409): return "This turn changed before the action arrived. Refresh and try again."
        case .http(429): return "Too many requests. Wait a moment and try again."
        case .http(let code): return "The server could not complete this request (\(code))."
        }
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

/// Native HTTP/SSE adapter for the existing /v1/agents contract. No embedded
/// runtime, model credentials, or execution environment is owned by this client.
public final class ManagedClient: @unchecked Sendable {
    private let credential: AccountCredential
    private let session: URLSession
    public init(credential: AccountCredential, configuration: URLSessionConfiguration? = nil) {
        self.credential = credential
        let config = configuration ?? URLSessionConfiguration.ephemeral
        config.httpShouldSetCookies = false
        config.httpCookieStorage = nil
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
            let encoder = JSONEncoder(); encoder.outputFormatting = [.withoutEscapingSlashes]
            request.httpBody = try encoder.encode(body)
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        if let idempotencyKey { request.setValue(idempotencyKey, forHTTPHeaderField: "Idempotency-Key") }
        return request
    }
    public func json(path: String, method: String = "GET", body: JSON? = nil, idempotencyKey: String? = nil) async throws -> JSON {
        let (data, response) = try await session.data(for: request(path: path, method: method, body: body, idempotencyKey: idempotencyKey))
        guard let response = response as? HTTPURLResponse else { throw APIError.invalidResponse }
        guard (200..<300).contains(response.statusCode) else {
            if response.statusCode == 409, data.count <= 64 * 1024,
               (try? JSONDecoder().decode(JSON.self, from: data)["error"].string) == "agent_deleting" {
                throw APIError.agentDeleting
            }
            throw APIError.http(response.statusCode)
        }
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
                             updatedAt: summary["updated_at"].number, turnCount: Int(summary["turn_count"].number),
                             mayHaveScheduledJobs: summary["may_have_scheduled_jobs"] != .bool(false))
        }
    }
    public static func agentPath(_ id: String) throws -> String {
        guard !id.isEmpty, id.count <= 128, id.utf8.allSatisfy({ (48...57).contains($0) || (65...90).contains($0) || (97...122).contains($0) || [45, 95].contains($0) }) else { throw APIError.invalidResponse }
        return "/v1/agents/" + id
    }
    public func state(_ id: String) async throws -> JSON { try await json(path: Self.agentPath(id)) }
    public func scheduledJobs(_ agentID: String) async throws -> [ScheduledJob] {
        let body = try await json(path: Self.agentPath(agentID) + "/triggers")
        guard case .array(let values) = body["data"] else { throw APIError.invalidResponse }
        let jobs = try values.map { try ScheduledJob($0, agentID: agentID) }
        guard Set(jobs.map(\.id)).count == jobs.count else { throw APIError.invalidResponse }
        return jobs
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
    public func history(_ id: String, before: Cursor? = nil) async throws -> EventPage {
        let path = try Self.agentPath(id) + "/events/history?limit=128" + (before.map { "&before=" + $0.rawValue } ?? "")
        return try EventPage(try await json(path: path))
    }
    @discardableResult
    public func command(_ command: AgentCommand) async throws -> JSON {
        let spec = try command.requestSpec()
        return try await json(path: spec.path, method: "POST", body: spec.body, idempotencyKey: spec.key)
    }
    /// Upload bounded chunks, retaining the attachment ID across retries. The
    /// service owns multipart state; credentials and upload IDs never enter a turn.
    public func uploadVideo(agentID: String, attachment: MessageAttachment, source: URL,
                            isCancelled: @Sendable () async -> Bool = { false }) async throws -> String {
        guard attachment.isVideo, source.isFileURL else { throw AttachmentError.invalidReference }
        let values = try source.resourceValues(forKeys: [.fileSizeKey, .isRegularFileKey, .isSymbolicLinkKey])
        guard values.isRegularFile == true, values.isSymbolicLink != true, values.fileSize == attachment.byteCount else { throw AttachmentError.invalidReference }
        let path = try Self.agentPath(agentID) + "/attachments/" + attachment.id.lowercased()
        let receipt = try await json(path: path, method: "POST", body: .object([
            "name": .string(attachment.name), "media_type": .string(attachment.mediaType), "size": .number(Double(attachment.byteCount))]))
        let filePath = receipt["path"].string
        _ = try attachment.originalVideoContent(path: filePath)
        guard receipt["size"].number == Double(attachment.byteCount) else { throw APIError.invalidResponse }
        if receipt["complete"] == .bool(true) { return filePath }
        let partSize = 8 * 1024 * 1024
        let number = receipt["next_part"].number
        let count = (attachment.byteCount - 1) / partSize + 1
        guard receipt["part_size"].number == Double(partSize), number >= 1, number <= Double(count + 1), number.rounded(.down) == number else { throw APIError.invalidResponse }
        let file = try FileHandle(forReadingFrom: source)
        defer { try? file.close() }
        var part = Int(number)
        try file.seek(toOffset: UInt64(min(attachment.byteCount, (part - 1) * partSize)))
        while part <= count {
            try Task.checkCancellation()
            if await isCancelled() { throw CancellationError() }
            let expected = min(partSize, attachment.byteCount - (part - 1) * partSize)
            var bytes = Data()
            while bytes.count < expected {
                guard let chunk = try file.read(upToCount: expected - bytes.count), !chunk.isEmpty else { throw AttachmentError.unavailable }
                bytes.append(chunk)
            }
            var request = try request(path: path + "/parts/" + String(part), method: "PUT")
            request.timeoutInterval = 120
            request.setValue("application/octet-stream", forHTTPHeaderField: "Content-Type")
            let (_, response) = try await session.upload(for: request, from: bytes)
            guard let response = response as? HTTPURLResponse else { throw APIError.invalidResponse }
            guard response.statusCode == 200 else { throw APIError.http(response.statusCode) }
            part += 1
        }
        try Task.checkCancellation()
        if await isCancelled() { throw CancellationError() }
        let complete = try await json(path: path + "/complete", method: "POST")
        guard complete["complete"] == .bool(true), complete["path"].string == filePath,
              complete["size"].number == Double(attachment.byteCount) else { throw APIError.invalidResponse }
        return filePath
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
    public func create(requestID: String) async throws -> String {
        // The managed contract uses an absent body for default settings; {} is invalid.
        let body = try await json(path: "/v1/agents", method: "POST", idempotencyKey: requestID)
        let id = body["agent_id"].string
        _ = try Self.agentPath(id)
        return id
    }
    #if !os(Linux)
    public func stream(_ id: String, after cursor: Cursor,
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
        // Cancelling observation releases its HTTP stream immediately, including
        // when an idle reader is waiting for the next keepalive.
        try await withTaskCancellationHandler {
            var parser = SSEParser()
            for try await byte in bytes {
                try Task.checkCancellation()
                if let frame = try parser.append(byte: byte) { await receive(frame) }
            }
        } onCancel: { task.cancel() }
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
    public var images: [JSON] = []
    public let kind: Kind
    public let requestID: String
    public init(agentID: String, turnID: String = "", input: String = "", kind: Kind, requestID: String = UUID().uuidString) {
        self.agentID = agentID; self.turnID = turnID; self.input = input; self.kind = kind; self.requestID = requestID
    }
    public func requestSpec() throws -> (path: String, body: JSON?, key: String?) {
        let path = try ManagedClient.agentPath(agentID) + "/turns"
        let content: JSON = images.isEmpty ? .string(input) : .array(
            (input.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? [] : [.object(["type": .string("text"), "text": .string(input)])]) + images
        )
        let body: JSON = .object(kind == .followUp ? ["id": .string(requestID), "input": content] : ["input": content])
        if kind != .stop {
            let encoder = JSONEncoder(); encoder.outputFormatting = [.withoutEscapingSlashes]
            guard try encoder.encode(body).count <= 1024 * 1024 else { throw APIError.messageTooLarge }
        }
        if kind == .followUp { return (path, body, "inbox:" + requestID) }
        guard !turnID.isEmpty, turnID.range(of: #"^[A-Za-z0-9._:-]{1,128}$"#, options: .regularExpression) != nil,
              let segment = turnID.addingPercentEncoding(withAllowedCharacters: .alphanumerics) else { throw APIError.invalidResponse }
        return (path + "/" + segment + (kind == .steer ? "/steer" : "/cancel"), kind == .steer ? body : nil, nil)
    }
}
