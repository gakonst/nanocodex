import Foundation
import InboxCore

public struct ManagedError: Error, LocalizedError, Sendable {
    public var code: String
    public var message: String
    public var status: Int?
    public var retryAt: Date?
    public init(code: String, message: String, status: Int? = nil, retryAt: Date? = nil) {
        self.code = code; self.message = message; self.status = status; self.retryAt = retryAt
    }
    public var errorDescription: String? { message }
    static let invalidResponse = ManagedError(code: "invalid_response", message: "Nanocodex returned an unexpected response. Please try again.")
    static let cancelled = ManagedError(code: "cancelled", message: "This connection is closed.")
}

func serviceOrigin(_ url: URL) throws -> URL {
    guard let components = URLComponents(url: url, resolvingAgainstBaseURL: false), let host = components.host,
          components.user == nil, components.password == nil, components.query == nil, components.fragment == nil,
          components.path.isEmpty || components.path == "/",
          components.scheme == "https" else {
        throw ManagedError(code: "invalid_origin", message: "Use an HTTPS service address.")
    }
    var origin = components; origin.path = ""; origin.host = host.lowercased(); origin.scheme = components.scheme?.lowercased()
    if (origin.scheme == "https" && origin.port == 443) || (origin.scheme == "http" && origin.port == 80) { origin.port = nil }
    return origin.url!
}

private final class RedirectBlocker: NSObject, URLSessionTaskDelegate, @unchecked Sendable {
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) { completionHandler(nil) }

    func urlSession(_ session: URLSession, task: URLSessionTask, didFinishCollecting metrics: URLSessionTaskMetrics) {
        guard voiceTimingEnabled,
              let operation = task.originalRequest?.url?.lastPathComponent, ["start", "stop", "delegate", "calls"].contains(operation),
              let transaction = metrics.transactionMetrics.last else { return }
        func milliseconds(_ start: Date?, _ end: Date?) -> Int {
            guard let start, let end else { return 0 }
            return Int(end.timeIntervalSince(start) * 1_000)
        }
        voiceTiming("http \(operation) status=\((task.response as? HTTPURLResponse)?.statusCode ?? 0) reused=\(transaction.isReusedConnection) connect_ms=\(milliseconds(transaction.connectStartDate, transaction.connectEndDate)) server_ms=\(milliseconds(transaction.requestEndDate, transaction.responseStartDate)) total_ms=\(Int(metrics.taskInterval.duration * 1_000))")
    }
}

final class HTTPTransport: @unchecked Sendable {
    let origin: URL
    let session: URLSession
    private let redirects = RedirectBlocker()
    init(origin: URL, configuration: URLSessionConfiguration? = nil) throws {
        self.origin = try serviceOrigin(origin)
        let config = configuration ?? .ephemeral
        config.httpCookieStorage = nil; config.httpShouldSetCookies = false; config.urlCache = nil
        config.requestCachePolicy = .reloadIgnoringLocalCacheData
        config.timeoutIntervalForRequest = 20; config.timeoutIntervalForResource = 86_400
        config.httpMaximumConnectionsPerHost = 8
        session = URLSession(configuration: config, delegate: redirects, delegateQueue: nil)
    }
    func request(_ path: String, method: String = "GET", body: Data? = nil, key: String? = nil, cookie: String? = nil,
                 idempotencyKey: String? = nil, streaming: Bool = false) throws -> URLRequest {
        guard path.hasPrefix("/"), !path.hasPrefix("//"), let url = URL(string: path, relativeTo: origin)?.absoluteURL,
              url.scheme == origin.scheme, url.host == origin.host, url.port == origin.port else { throw ManagedError.invalidResponse }
        var request = URLRequest(url: url); request.httpMethod = method; request.httpBody = body
        request.timeoutInterval = streaming ? 45 : 20
        request.setValue(streaming ? "text/event-stream" : "application/json", forHTTPHeaderField: "Accept")
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        if let key { request.setValue("Bearer \(key)", forHTTPHeaderField: "Authorization") }
        if let cookie { request.setValue(cookie, forHTTPHeaderField: "Cookie") }
        if cookie != nil || path.hasPrefix("/v1/auth/") { request.setValue(origin.absoluteString, forHTTPHeaderField: "Origin") }
        if let idempotencyKey { request.setValue(idempotencyKey, forHTTPHeaderField: "Idempotency-Key") }
        return request
    }
    func data(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
        let operation = request.url?.lastPathComponent ?? "unknown"
        let timingOperation = ["start", "stop", "delegate", "calls", "prefetch", "context"].contains(operation) ? operation : "request"
        voiceTiming("http.begin \(timingOperation)")
        do {
            let (data, response) = try await withThrowingTaskGroup(of: (Data, URLResponse).self) { group in
                group.addTask { try await self.session.data(for: request) }
                group.addTask { try await Task.sleep(for: .seconds(20)); throw URLError(.timedOut) }
                defer { group.cancelAll() }
                return try await group.next()!
            }
            guard let http = response as? HTTPURLResponse, data.count <= 32 * 1024 * 1024 else { throw ManagedError.invalidResponse }
            guard (200..<300).contains(http.statusCode) else { throw responseError(data, response: http) }
            return (data, http)
        } catch is CancellationError { throw CancellationError() }
        catch let error as ManagedError {
            voiceTiming("http.rejected \(timingOperation) status=\(error.status ?? 0) code=\(error.code)")
            throw error
        }
        catch {
            if Task.isCancelled { throw CancellationError() }
            voiceTiming("http.failure \(timingOperation) code=\((error as NSError).code)")
            throw ManagedError(code: "network_error", message: "We could not reach Nanocodex. Check your connection and try again.")
        }
    }
    func json<T: Decodable>(_ request: URLRequest, as type: T.Type = T.self) async throws -> T {
        let (data, _) = try await data(request)
        do { return try JSONDecoder().decode(type, from: data) } catch { throw ManagedError.invalidResponse }
    }
    func close() { session.invalidateAndCancel() }
}

func responseError(_ data: Data, response: HTTPURLResponse) -> ManagedError {
    let body = (try? JSONDecoder().decode(JSON.self, from: data)) ?? .null
    let rawCode = body["error"].string
    let code = rawCode.range(of: "^[a-z][a-z0-9_]{0,63}$", options: .regularExpression) == nil ? "request_failed" : rawCode
    let retry = Double(response.value(forHTTPHeaderField: "Retry-After") ?? "")
        ?? (body["retry_after"] == .null ? nil : body["retry_after"].number)
    var message = body["message"].string
    if message.isEmpty {
        switch response.statusCode {
        case 401: message = "Your sign-in expired. Sign in again to continue."
        case 403: message = "This account does not have permission for that action."
        case 404: message = "This thread is no longer available."
        case 429: message = "Please wait before trying again."
        default: message = "Nanocodex could not complete that request. Please try again."
        }
    }
    for pattern in ["ncx_live_[A-Za-z0-9_-]+", "nanocodex_account=[A-Za-z0-9_-]+"] {
        message = message.replacingOccurrences(of: pattern, with: "[redacted]", options: .regularExpression)
    }
    return ManagedError(code: code, message: String(message.prefix(500)), status: response.statusCode,
                        retryAt: retry.map { Date().addingTimeInterval(min(max($0, 1), 3600)) })
}

func validatedAgentID(_ id: String) throws -> String {
    guard UUID(uuidString: id) != nil else { throw ManagedError(code: "invalid_agent", message: "This thread address is invalid.") }
    return id
}

func validatedTurnID(_ id: String) throws -> String {
    guard id.range(of: "^[A-Za-z0-9._:-]{1,128}$", options: .regularExpression) != nil else { throw ManagedError.invalidResponse }
    return id
}
