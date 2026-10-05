import Foundation
import CryptoKit
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif

public struct GoogleAuthorization: Sendable {
    public let url: URL
    public let attemptID: String
}

/// Native account sign-in, independent of the Google Workspace connector. The
/// verifier, temporary account cookie and uncommitted API key stay in this actor.
public actor GoogleAuth {
    private struct Attempt: Sendable {
        var id: String
        var verifier: String
        var expiresAt: Date
        var cookie: String?
        var key: AccountCredential?
        var keyID: String?
        var committed = false
    }
    private let transport: SMSAuthTransport
    private let label: String
    private var attempt: Attempt?
    private var generation = 0
    private var queue: Task<Void, Never>?

    public init(origin: String = "https://nanocodex.gakonst.workers.dev", deviceName: String, configuration: URLSessionConfiguration? = nil) throws {
        transport = try SMSAuthTransport(origin: origin, configuration: configuration)
        label = String("Nanocodex on \(deviceName)".prefix(120))
    }

    private func serial<T: Sendable>(_ work: @escaping @Sendable () async throws -> T) async throws -> T {
        let previous = queue
        let task = Task { await previous?.value; return try await work() }
        queue = Task { _ = try? await task.value }
        return try await task.value
    }
    private func current(_ expected: Int) throws {
        guard expected == generation else { throw CancellationError() }
    }
    public func start() async throws -> GoogleAuthorization {
        generation += 1; let expected = generation
        return try await serial { try await self.startStep(generation: expected) }
    }
    private func startStep(generation: Int) async throws -> GoogleAuthorization {
        try current(generation)
        try await discard(revoke: true)
        try current(generation)
        // Swift's system random generator uses the OS cryptographic RNG.
        var random = SystemRandomNumberGenerator()
        let verifier = Self.base64URL(Data((0..<32).map { _ in UInt8.random(in: .min ... .max, using: &random) }))
        let challenge = Self.base64URL(Data(SHA256.hash(data: Data(verifier.utf8))))
        let (data, _) = try await request("/v1/auth/google/start", body: ["mode": "native", "code_challenge": challenge])
        struct Reply: Decodable { let attempt_id: String; let authorization_url: String; let expires_in: Int }
        guard let reply = try? JSONDecoder().decode(Reply.self, from: data),
              reply.attempt_id.range(of: "^[A-Za-z0-9_-]{43}$", options: .regularExpression) != nil,
              (1...3600).contains(reply.expires_in) else { throw SMSAuthError.invalidResponse }
        attempt = Attempt(id: reply.attempt_id, verifier: verifier, expiresAt: Date().addingTimeInterval(Double(reply.expires_in)))
        // Never open an arbitrary URL returned by a compromised/misconfigured endpoint.
        guard let parts = URLComponents(string: reply.authorization_url), let url = parts.url,
              parts.scheme == "https", parts.user == nil, parts.password == nil, parts.fragment == nil,
              parts.path == "/v1/auth/google/authorize",
              parts.queryItems == [URLQueryItem(name: "attempt_id", value: reply.attempt_id)],
              URLComponents(string: transport.origin)?.host == parts.host,
              URLComponents(string: transport.origin)?.port == parts.port else { throw SMSAuthError.invalidResponse }
        try current(generation)
        return GoogleAuthorization(url: url, attemptID: reply.attempt_id)
    }

    public func finish(completionCode: String?) async throws -> AccountCredential {
        let expected = generation
        return try await serial { try await self.finishStep(completionCode: completionCode, generation: expected) }
    }
    private func finishStep(completionCode: String?, generation: Int) async throws -> AccountCredential {
        try current(generation)
        guard var pending = attempt else { throw SMSAuthError(code: "not_started", message: "Start Google sign-in again.") }
        if pending.cookie == nil {
            guard pending.expiresAt > Date() else { throw SMSAuthError(code: "expired", message: "Google sign-in expired. Please try again.") }
            let proof = ["attempt_id": pending.id, "code_verifier": pending.verifier]
            let (data, _) = try await request("/v1/auth/google/status", body: proof)
            struct Status: Decodable { let status: String; let error: String? }
            guard let status = try? JSONDecoder().decode(Status.self, from: data) else { throw SMSAuthError.invalidResponse }
            switch status.status {
            case "ready": break
            case "cancelled": throw CancellationError()
            case "pending": throw SMSAuthError(code: "pending", message: "Google sign-in is not finished. Please try again.")
            case "failed":
                if status.error == "google_access_denied" { throw CancellationError() }
                throw Self.failure(status.error ?? "google_sign_in_failed")
            default: throw SMSAuthError.invalidResponse
            }
            try current(generation)
            guard let completionCode, completionCode.range(of: "^[A-Za-z0-9_-]{43}$", options: .regularExpression) != nil else { throw SMSAuthError.invalidResponse }
            let (_, response) = try await request("/v1/auth/google/complete", body: proof.merging(["completion_code": completionCode]) { _, value in value })
            guard let header = response.value(forHTTPHeaderField: "Set-Cookie"),
                  let match = header.range(of: "(?:^|,\\s*)nanocodex_account=s_[A-Za-z0-9_-]{43}(?=;|$)", options: .regularExpression) else { throw SMSAuthError.invalidResponse }
            pending.cookie = String(header[match]).trimmingCharacters(in: CharacterSet(charactersIn: ", "))
            attempt = pending
            try current(generation)
        }
        if pending.key == nil {
            let (data, _) = try await request("/v1/api-keys", body: ["label": label], cookie: pending.cookie)
            struct Reply: Decodable { let api_key: String }
            guard let reply = try? JSONDecoder().decode(Reply.self, from: data) else { throw SMSAuthError.invalidResponse }
            pending.key = try AccountCredential(origin: transport.origin, apiKey: reply.api_key)
            pending.keyID = String(reply.api_key.dropFirst("ncx_live_".count).prefix(12))
            // Publish the key into private cleanup state before cancellation checks.
            attempt = pending
            try current(generation)
        }
        return pending.key!
    }
    /// Call immediately after the credential is securely persisted/adopted.
    public func complete() async throws {
        attempt?.committed = true
        try await serial { try await self.discard(revoke: false) }
    }
    public func cancel() async throws {
        generation += 1
        try await serial { try await self.discard(revoke: true) }
    }
    private func discard(revoke: Bool) async throws {
        guard let pending = attempt else { return }
        if revoke, !pending.committed, let id = pending.keyID {
            do { _ = try await request("/v1/api-keys/\(id)", method: "DELETE", cookie: pending.cookie) }
            catch let error as SMSAuthError where error.status == 404 { /* Already revoked. */ }
        }
        // An exchanged attempt has already been consumed. Logout removes its
        // temporary session; cancelling an unexchanged attempt invalidates proof.
        if pending.cookie == nil {
            do { _ = try await request("/v1/auth/google/cancel", body: ["attempt_id": pending.id, "code_verifier": pending.verifier]) }
            catch let error as SMSAuthError where error.status == 404 || error.status == 410 || error.code == "invalid_or_expired_google_attempt" { /* Expired or already consumed. */ }
        }
        attempt = nil
        if let cookie = pending.cookie { _ = try? await request("/v1/auth/logout", cookie: cookie) }
    }
    private static func base64URL(_ data: Data) -> String {
        data.base64EncodedString().replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
    }
    private static func failure(_ code: String, status: Int? = nil, retryAt: Date? = nil) -> SMSAuthError {
        let messages = [
            "google_auth_unavailable": "Google sign-in is temporarily unavailable. Try again shortly or use your phone number.",
            "google_sign_in_unavailable": "Google sign-in is temporarily unavailable. Try again shortly or use your phone number.",
            "identity_conflict": "This Google account cannot be used for this sign-in. Sign in with your existing method.",
            "google_identity_conflict": "This Google account cannot be used for this sign-in. Sign in with your existing method.",
            "rate_limited": "Please wait before trying Google sign-in again.",
            "network_error": "We could not reach Nanocodex. Check your connection and try again.",
            "forbidden": "This account cannot authorize the app. Contact your account administrator."
        ]
        return SMSAuthError(code: code, message: messages[code] ?? "We could not finish Google sign-in. Please try again.", status: status, retryAt: retryAt)
    }
    private func request(_ path: String, method: String = "POST", body: [String: String]? = nil, cookie: String? = nil) async throws -> (Data, HTTPURLResponse) {
        do { return try await transport.data(transport.request(path, method: method, body: body.map { try JSONEncoder().encode($0) }, cookie: cookie)) }
        catch let error as SMSAuthError { throw Self.failure(error.code, status: error.status, retryAt: error.retryAt) }
    }
}
