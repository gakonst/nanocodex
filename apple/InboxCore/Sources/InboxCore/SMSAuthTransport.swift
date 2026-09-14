import Foundation
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif

public struct SMSAuthError: LocalizedError, Sendable {
    public let code: String
    public let message: String
    public let status: Int?
    public let retryAt: Date?
    init(code: String, message: String, status: Int? = nil, retryAt: Date? = nil) {
        self.code = code; self.message = message; self.status = status; self.retryAt = retryAt
    }
    public var errorDescription: String? { message }
    static let invalidResponse = SMSAuthError(code: "invalid_response", message: "Nanocodex returned an unexpected response. Please try again.")
}

/// The SMS session never enters the shared cookie jar, cache, URLs, or logs.
final class SMSAuthTransport: @unchecked Sendable {
    let origin: String
    private let session: URLSession
    init(origin: String, configuration: URLSessionConfiguration? = nil) throws {
        self.origin = try AccountCredential.normalizedOrigin(origin)
        let config = configuration ?? .ephemeral
        config.httpCookieStorage = nil; config.httpShouldSetCookies = false; config.urlCache = nil
        config.requestCachePolicy = .reloadIgnoringLocalCacheData
        config.timeoutIntervalForRequest = 20; config.timeoutIntervalForResource = 20
        session = URLSession(configuration: config, delegate: NoRedirects(), delegateQueue: nil)
    }
    deinit { session.invalidateAndCancel() }
    func request(_ path: String, method: String, body: Data?, cookie: String?) throws -> URLRequest {
        guard path.hasPrefix("/v1/"), !path.contains(".."), !path.contains("?"), !path.contains("#"),
              let url = URL(string: origin + path) else { throw SMSAuthError.invalidResponse }
        var request = URLRequest(url: url)
        request.httpMethod = method; request.httpBody = body
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.setValue(origin, forHTTPHeaderField: "Origin")
        if let cookie { request.setValue(cookie, forHTTPHeaderField: "Cookie") }
        return request
    }
    func data(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
        do {
            let (data, response) = try await session.data(for: request)
            guard let response = response as? HTTPURLResponse, data.count <= 16_384 else { throw SMSAuthError.invalidResponse }
            guard (200..<300).contains(response.statusCode) else {
                let body = (try? JSONDecoder().decode(JSON.self, from: data)) ?? .null
                let rawRetry = Double(response.value(forHTTPHeaderField: "Retry-After") ?? "") ?? body["retry_after"].number
                let retry = rawRetry.isFinite && rawRetry > 0 ? min(ceil(rawRetry), 3600) : 60
                let limited = response.statusCode == 429 || body["error"].string == "rate_limited"
                // Only SMSAuth's known error messages are shown to the user.
                throw SMSAuthError(code: limited ? "rate_limited" : body["error"].string,
                                   message: "Sign-in failed.", status: response.statusCode,
                                   retryAt: limited ? Date().addingTimeInterval(retry) : nil)
            }
            return (data, response)
        } catch is CancellationError { throw CancellationError() }
        catch let error as SMSAuthError { throw error }
        catch {
            throw SMSAuthError(code: "network_error", message: "We could not reach Nanocodex. Check your connection and try again.")
        }
    }
}
