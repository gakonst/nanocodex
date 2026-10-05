import XCTest
import Foundation
import CryptoKit
@testable import InboxCore

/// HTTP-boundary coverage for the native actor's private credential lifecycle.
/// URLProtocol stands in for the account service because the OS browser and
/// Keychain journey requires an Apple device. The real server's Google identity
/// verification and account isolation are covered by its public route journeys.
final class GoogleAuthJourneyTests: XCTestCase {
    func testSignInCommitsKeyAndDiscardsTemporaryCookie() async throws {
        let fixture = GoogleAccountFixture()
        let auth = try fixture.auth()
        let authorization = try await auth.start()
        XCTAssertEqual(authorization.url.absoluteString, fixture.origin + "/v1/auth/google/authorize?attempt_id=" + fixture.attemptID)
        let credential = try await auth.finish(completionCode: fixture.completionCode)
        XCTAssertEqual(credential.apiKey, fixture.apiKey)
        // A real caller saves to Keychain before complete().
        try await auth.complete()
        try await auth.cancel()
        XCTAssertEqual(fixture.paths, ["/v1/auth/google/start", "/v1/auth/google/status", "/v1/auth/google/complete", "/v1/api-keys", "/v1/auth/logout"])
        XCTAssertNil(HTTPCookieStorage.shared.cookies(for: URL(string: fixture.origin)!)?.first)
        XCTAssertTrue(fixture.violations.isEmpty, fixture.violations.joined(separator: "; "))
    }

    func testCallbackPossessionRequiredAndUnsavedKeyRevoked() async throws {
        let fixture = GoogleAccountFixture()
        let auth = try fixture.auth()
        _ = try await auth.start()
        do { _ = try await auth.finish(completionCode: nil); XCTFail("Missing OS callback must not exchange an account session") }
        catch { XCTAssertEqual((error as? SMSAuthError)?.code, "invalid_response") }
        XCTAssertFalse(fixture.paths.contains("/v1/auth/google/complete"))
        _ = try await auth.finish(completionCode: fixture.completionCode)
        try await auth.cancel()
        XCTAssertEqual(Array(fixture.paths.suffix(2)), ["/v1/api-keys/AAAAAAAAAAAA", "/v1/auth/logout"])
        XCTAssertTrue(fixture.violations.isEmpty, fixture.violations.joined(separator: "; "))
    }

    func testFailedRevocationIsRetainedUntilRetryAndExpiredAttemptCanRestart() async throws {
        let fixture = GoogleAccountFixture()
        let auth = try fixture.auth()
        _ = try await auth.start()
        _ = try await auth.finish(completionCode: fixture.completionCode)
        fixture.failNextRevocation = true
        do { try await auth.cancel(); XCTFail("Failed cleanup must remain retryable") }
        catch { XCTAssertEqual((error as? SMSAuthError)?.status, 503) }
        XCTAssertFalse(fixture.paths.contains("/v1/auth/logout"))
        try await auth.cancel()
        XCTAssertEqual(fixture.paths.filter { $0 == "/v1/api-keys/AAAAAAAAAAAA" }.count, 2)
        _ = try await auth.start()
        fixture.expireAttempt = true
        try await auth.cancel()
        fixture.expireAttempt = false
        _ = try await auth.start()
        try await auth.cancel()
        XCTAssertEqual(fixture.paths.filter { $0 == "/v1/auth/google/start" }.count, 3)
        XCTAssertTrue(fixture.violations.isEmpty, fixture.violations.joined(separator: "; "))
    }
}

private final class GoogleAccountFixture: @unchecked Sendable {
    let origin = "https://" + UUID().uuidString.lowercased() + ".example.test"
    let attemptID = String(repeating: "a", count: 43)
    let completionCode = String(repeating: "b", count: 43)
    let apiKey = "ncx_live_AAAAAAAAAAAA_" + String(repeating: "c", count: 43)
    let cookie = "nanocodex_account=s_" + String(repeating: "d", count: 43)
    var failNextRevocation = false
    var expireAttempt = false
    private(set) var paths: [String] = []
    private(set) var violations: [String] = []
    private var challenge = ""

    func auth() throws -> GoogleAuth {
        GoogleJourneyProtocol.register(self)
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [GoogleJourneyProtocol.self]
        return try GoogleAuth(origin: origin, deviceName: "Synthetic test device", configuration: configuration)
    }
    func respond(_ request: URLRequest) -> (Int, [String: String], Data) {
        let path = request.url!.path
        paths.append(path)
        func check(_ condition: Bool, _ message: String) { if !condition { violations.append(message) } }
        check(request.value(forHTTPHeaderField: "Origin") == origin, "Missing exact Origin")
        check(request.value(forHTTPHeaderField: "Authorization") == nil, "Unexpected Authorization")
        check(request.url?.query == nil && request.url?.fragment == nil, "Private API URL has query/fragment")
        let body = (request.httpBody.flatMap { try? JSONSerialization.jsonObject(with: $0) }) as? [String: String] ?? [:]
        func json(_ value: [String: Any], status: Int = 200, headers: [String: String] = [:]) -> (Int, [String: String], Data) {
            (status, headers, try! JSONSerialization.data(withJSONObject: value))
        }
        if path.hasPrefix("/v1/auth/google/") {
            check(request.value(forHTTPHeaderField: "Cookie") == nil, "Account cookie sent before exchange")
            check(request.httpMethod == "POST", "Google auth must use POST")
            if path == "/v1/auth/google/start" {
                check(body["mode"] == "native", "Expected native mode")
                challenge = body["code_challenge"] ?? ""
                check(challenge.count == 43, "Expected S256 challenge")
                check(body["code_verifier"] == nil, "Verifier disclosed to start")
                return json(["attempt_id": attemptID, "authorization_url": origin + "/v1/auth/google/authorize?attempt_id=" + attemptID, "expires_in": 300])
            }
            let verifier = body["code_verifier"] ?? ""
            let hash = Data(SHA256.hash(data: Data(verifier.utf8))).base64EncodedString().replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
            check(hash == challenge && body["attempt_id"] == attemptID, "Proof does not match start")
            if path == "/v1/auth/google/status" { return json(["status": "ready"]) }
            if path == "/v1/auth/google/complete" {
                check(body["completion_code"] == completionCode, "Missing callback possession proof")
                return json(["user": ["id": "synthetic-user"]], headers: ["Set-Cookie": cookie + "; Path=/; Secure; HttpOnly"])
            }
            if path == "/v1/auth/google/cancel" {
                if expireAttempt { return json(["error": "invalid_or_expired_google_attempt"], status: 400) }
                return (204, [:], Data())
            }
        }
        check(request.value(forHTTPHeaderField: "Cookie") == cookie, "Missing private account cookie")
        if path == "/v1/api-keys" { return json(["api_key": apiKey]) }
        if path == "/v1/api-keys/AAAAAAAAAAAA" {
            check(request.httpMethod == "DELETE", "Unused key must be revoked with DELETE")
            if failNextRevocation { failNextRevocation = false; return json(["error": "unavailable"], status: 503) }
            return (204, [:], Data())
        }
        if path == "/v1/auth/logout" { return (204, [:], Data()) }
        violations.append("Unexpected request " + path)
        return json(["error": "not_found"], status: 404)
    }
}

private final class GoogleJourneyProtocol: URLProtocol {
    private static let lock = NSLock()
    private static var fixtures: [String: GoogleAccountFixture] = [:]
    static func register(_ fixture: GoogleAccountFixture) { lock.lock(); defer { lock.unlock() }; fixtures[fixture.origin] = fixture }
    override class func canInit(with request: URLRequest) -> Bool { request.url?.host?.hasSuffix(".example.test") == true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        Self.lock.lock(); let fixture = Self.fixtures["https://" + (request.url?.host ?? "")]; Self.lock.unlock()
        guard let fixture else { client?.urlProtocol(self, didFailWithError: URLError(.badURL)); return }
        // Foundation may expose POST payloads as a stream to URLProtocol.
        var request = request
        if request.httpBody == nil, let stream = request.httpBodyStream {
            stream.open(); defer { stream.close() }
            var data = Data(); var bytes = [UInt8](repeating: 0, count: 1024)
            while stream.hasBytesAvailable { let count = stream.read(&bytes, maxLength: bytes.count); if count <= 0 { break }; data.append(contentsOf: bytes.prefix(count)) }
            request.httpBody = data
        }
        let (status, headers, data) = fixture.respond(request)
        print("Native Google journey: \(request.httpMethod ?? "?") \(request.url!.path) -> \(status)")
        let response = HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: "HTTP/1.1", headerFields: headers)!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: data)
        client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() {}
}
