import Foundation
import CryptoKit
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif

/// In-memory, account-separated authority for managed requests and screen viewer admission.
/// The server binds each short-lived token to the original account credential.
public enum ManagedAccess {
    private final class Entry {
        let token: String
        let until: TimeInterval
        init(token: String, until: TimeInterval) { self.token = token; self.until = until }
    }
    private static let header = "x-nanocodex-access"
    private static let lock = NSLock()
    private static let entries: NSCache<NSString, Entry> = {
        let cache = NSCache<NSString, Entry>(); cache.countLimit = 64; return cache
    }()

    private static func identity(_ request: URLRequest) -> NSString? {
        guard let url = request.url, url.scheme == "https", let host = url.host,
              eligible(request, path: url.path),
              request.value(forHTTPHeaderField: "Accept")?.contains("text/event-stream") != true,
              let authorization = request.value(forHTTPHeaderField: "Authorization"), authorization.hasPrefix("Bearer ncx_live_") else { return nil }
        let scope = "https://\(host):\(url.port ?? 443)\n\(authorization)"
        return SHA256.hash(data: Data(scope.utf8)).map { String(format: "%02x", $0) }.joined() as NSString
    }

    private static func eligible(_ request: URLRequest, path: String) -> Bool {
        let method = request.httpMethod ?? "GET"
        if path == "/v1/account/hands/view" { return method == "GET" }
        if path == "/v1/account/hands/screens" || path == "/v1/account/hands/ice" {
            return method == (path.hasSuffix("/screens") ? "GET" : "POST")
                && request.value(forHTTPHeaderField: "Upgrade") == nil
        }
        return (path == "/v1/agents" || path.hasPrefix("/v1/agents/"))
            && !["ws", "events", "tool-host", "device-host", "sideband"].contains(request.url?.lastPathComponent ?? "")
            && request.value(forHTTPHeaderField: "Upgrade") == nil
    }

    /// Prepare authenticated screen admission before URLSession converts HTTPS
    /// to WSS. Publishers and live lease renewals are deliberately ineligible.
    public static func prepared(_ original: URLRequest) -> URLRequest {
        guard let identity = identity(original) else { return original }
        var request = original
        request.setValue(cachedToken(identity, now: ProcessInfo.processInfo.systemUptime), forHTTPHeaderField: header)
        return request
    }

    /// Only an explicit pre-admission rejection permits retrying a handshake.
    /// A post-admission socket failure must never retry or replay control input.
    public static func rejected(_ request: URLRequest, response: HTTPURLResponse) -> Bool {
        guard response.statusCode == 401, response.value(forHTTPHeaderField: "x-nanocodex-access-rejected") == "1",
              let token = request.value(forHTTPHeaderField: header), let url = request.url,
              var components = URLComponents(url: url, resolvingAgainstBaseURL: false) else { return false }
        var original = request
        if components.scheme == "wss" { components.scheme = "https"; original.url = components.url }
        guard let identity = identity(original) else { return false }
        invalidate(identity, token: token)
        return true
    }

    public static func clear() { lock.lock(); defer { lock.unlock() }; entries.removeAllObjects() }

    public static func data(for original: URLRequest, using session: URLSession) async throws -> (Data, URLResponse) {
        guard let identity = identity(original) else { return try await session.data(for: original) }
        let began = ProcessInfo.processInfo.systemUptime
        let token: String? = cachedToken(identity, now: began)
        var request = original
        request.setValue(token, forHTTPHeaderField: header)
        var result = try await session.data(for: request)
        if token != nil, let rejected = result.1 as? HTTPURLResponse, rejected.statusCode == 401,
           rejected.value(forHTTPHeaderField: "x-nanocodex-access-rejected") == "1" {
            invalidate(identity, token: token!)
            request.setValue(nil, forHTTPHeaderField: header)
            // Ingress rejected the snapshot before admission. Retry the same operation once.
            result = try await session.data(for: request)
        }
        if let response = result.1 as? HTTPURLResponse { remember(identity, response: response, began: began) }
        return result
    }

    private static func cachedToken(_ identity: NSString, now: TimeInterval) -> String? {
        lock.lock(); defer { lock.unlock() }
        guard let value = entries.object(forKey: identity), value.until > now + 5 else { return nil }
        return value.token
    }
    private static func invalidate(_ identity: NSString, token: String) {
        lock.lock(); defer { lock.unlock() }
        if entries.object(forKey: identity)?.token == token { entries.removeObject(forKey: identity) }
    }
    private static func remember(_ identity: NSString, response: HTTPURLResponse, began: TimeInterval) {
        guard (200..<300).contains(response.statusCode), let token = response.value(forHTTPHeaderField: header),
              token.hasPrefix("ncx_access_v1."), token.utf8.count <= 16_384,
              let ttl = Double(response.value(forHTTPHeaderField: "x-nanocodex-access-ttl-ms") ?? ""),
              ttl.isFinite, ttl > 0, ttl <= 120_000 else { return }
        let until = began + ttl / 1000
        lock.lock(); defer { lock.unlock() }
        if let current = entries.object(forKey: identity), current.until >= until { return }
        entries.setObject(Entry(token: token, until: until), forKey: identity)
    }
}
