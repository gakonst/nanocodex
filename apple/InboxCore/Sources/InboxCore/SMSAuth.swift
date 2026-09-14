import Foundation
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif

public struct SMSChallenge: Codable, Equatable, Sendable {
    public var phone: String
    public var resendAt: Date
    public var expiresAt: Date
    public init(phone: String, resendAt: Date, expiresAt: Date) { self.phone = phone; self.resendAt = resendAt; self.expiresAt = expiresAt }
}

/// The temporary browser session and unused API key stay private to this actor.
/// Call complete only after OS credential storage succeeds; cancel revokes an unused key.
public actor SMSAuth {
    private struct Attempt: Sendable {
        var challenge: SMSChallenge
        var challengeID: String?
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

    public init(origin: String = "https://nanocodex.gakonst.workers.dev", deviceName: String = "iPhone", configuration: URLSessionConfiguration? = nil) throws {
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
        guard expected == generation else { throw SMSAuthError(code: "cancelled", message: "Sign-in was cancelled.") }
    }
    public func start(phone: String) async throws -> SMSChallenge {
        let normalized = phone.replacingOccurrences(of: "[\\s().-]", with: "", options: .regularExpression)
        guard normalized.range(of: "^\\+[1-9][0-9]{7,14}$", options: .regularExpression) != nil else {
            throw SMSAuthError(code: "invalid_phone", message: "Enter a valid phone number and check the selected country.")
        }
        generation += 1; let expected = generation
        return try await serial { try await self.startStep(phone: normalized, generation: expected) }
    }
    private func startStep(phone: String, generation: Int) async throws -> SMSChallenge {
        try current(generation)
        if let attempt, attempt.challenge.phone == phone, attempt.challenge.resendAt > Date() {
            throw SMSAuthError(code: "rate_limited", message: "Please wait before requesting another code.", retryAt: attempt.challenge.resendAt)
        }
        try await discard(revoke: true); try current(generation)
        let (data, _) = try await smsRequest("/v1/auth/sms/start", body: ["phone": phone])
        struct Reply: Decodable { var challenge_id: String; var expires_in: Int; var resend_after: Int }
        guard let reply = try? JSONDecoder().decode(Reply.self, from: data),
              reply.challenge_id.range(of: "^[A-Za-z0-9_-]{43}$", options: .regularExpression) != nil,
              (1...3600).contains(reply.expires_in), (0...3600).contains(reply.resend_after) else { throw SMSAuthError.invalidResponse }
        let challenge = SMSChallenge(phone: phone, resendAt: Date().addingTimeInterval(Double(reply.resend_after)), expiresAt: Date().addingTimeInterval(Double(reply.expires_in)))
        attempt = Attempt(challenge: challenge, challengeID: reply.challenge_id)
        try current(generation); return challenge
    }
    public func verify(code: String) async throws -> AccountCredential {
        let expected = generation
        return try await serial { try await self.verifyStep(code: code, generation: expected) }
    }
    private func verifyStep(code: String, generation: Int) async throws -> AccountCredential {
        try current(generation)
        guard var pending = attempt else { throw SMSAuthError(code: "not_started", message: "Request a text message code first.") }
        if pending.cookie == nil {
            guard pending.challenge.expiresAt > Date() else { throw SMSAuthError(code: "expired", message: "Your code has expired. Request a new code.") }
            let normalized = code.replacingOccurrences(of: "\\s", with: "", options: .regularExpression)
            guard normalized.range(of: "^[0-9]{6}$", options: .regularExpression) != nil else { throw SMSAuthError(code: "invalid_otp", message: "Enter the six-digit code from your text message.") }
            let (_, response) = try await smsRequest("/v1/auth/sms/verify", body: ["phone": pending.challenge.phone, "challenge_id": pending.challengeID ?? "", "code": normalized])
            guard let header = response.value(forHTTPHeaderField: "Set-Cookie"),
                  let match = header.range(of: "(?:^|,\\s*)nanocodex_account=s_[A-Za-z0-9_-]{43}(?=;|$)", options: .regularExpression) else { throw SMSAuthError.invalidResponse }
            pending.cookie = String(header[match]).trimmingCharacters(in: CharacterSet(charactersIn: ", "))
            pending.challengeID = nil; attempt = pending
            try current(generation)
        }
        if pending.key == nil {
            let (data, _) = try await smsRequest("/v1/api-keys", body: ["label": label], cookie: pending.cookie)
            struct Reply: Decodable { var api_key: String }
            guard let reply = try? JSONDecoder().decode(Reply.self, from: data),
                  reply.api_key.range(of: "^ncx_live_[A-Za-z0-9_-]{12}_[A-Za-z0-9_-]{43}$", options: .regularExpression) != nil else { throw SMSAuthError.invalidResponse }
            pending.key = try AccountCredential(origin: transport.origin, apiKey: reply.api_key)
            pending.keyID = String(reply.api_key.dropFirst("ncx_live_".count).prefix(12))
            // Retain the exact newly minted key before checking cancellation.
            // A racing cancel can now revoke it instead of leaking an unused key.
            attempt = pending; try current(generation)
        }
        return pending.key!
    }
    public func complete() async throws {
        // Commit before the first suspension. A simultaneous form dismissal may
        // cancel cleanup, but it must never revoke an already persisted key.
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
            do { _ = try await smsRequest("/v1/api-keys/\(id)", method: "DELETE", cookie: pending.cookie) }
            catch let error as SMSAuthError where error.status == 404 { /* Already revoked. */ }
        }
        // A successful credential save is final even if logout is unavailable.
        attempt = nil
        if let cookie = pending.cookie { _ = try? await smsRequest("/v1/auth/logout", cookie: cookie) }
    }
    private func smsRequest(_ path: String, method: String = "POST", body: [String: String]? = nil, cookie: String? = nil) async throws -> (Data, HTTPURLResponse) {
        do { return try await transport.data(transport.request(path, method: method, body: body.map { try JSONEncoder().encode($0) }, cookie: cookie)) }
        catch let error as SMSAuthError {
            let messages = [
                "invalid_phone": "Enter a valid phone number and check the selected country.",
                "invalid_otp": "Enter the six-digit code from your text message.",
                "invalid_or_expired_otp": "That code is incorrect or has expired. Try again or request a new code.",
                "sms_otp_unavailable": "Phone sign-in is temporarily unavailable. Please try again shortly.",
                "sms_delivery_failed": "We could not send your code. Check your number and try again.",
                "sms_verification_failed": "We could not check your code. Please try again.",
                "sms_identity_unavailable": "We could not finish signing you in. Please try again.",
                "wallet_unavailable": "Your account is still being prepared. Try the code again shortly.",
                "rate_limited": "Please wait before requesting another code.",
                "unauthorized": "Your sign-in expired. Request a new code.",
                "forbidden": "This account cannot authorize the app. Contact your account administrator.",
                "network_error": "We could not reach Nanocodex. Check your connection and try again.",
            ]
            throw SMSAuthError(code: error.code, message: messages[error.code] ?? "We could not finish signing you in. Please try again.", status: error.status, retryAt: error.retryAt)
        }
    }
}
