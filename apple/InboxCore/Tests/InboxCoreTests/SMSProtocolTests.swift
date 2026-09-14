import XCTest
@testable import InboxCore

final class SMSProtocolTests: XCTestCase {
    private let challenge = String(repeating: "c", count: 43)
    private let cookie = "nanocodex_account=s_" + String(repeating: "s", count: 43)

    func testUntrustedResponsesCannotAdvanceOrExposeCredentials() async throws {
        for invalidStage in ["/v1/auth/sms/start", "/v1/auth/sms/verify", "/v1/api-keys"] {
            var reached: [String] = []
            let fixture = try HTTPFixture { request in
                reached.append(request.path)
                if request.path == invalidStage { return .init(headers: ["Set-Cookie": "unrelated=secret"], body: "{}") }
                switch request.path {
                case "/v1/auth/sms/start": return .init(body: "{\"challenge_id\":\"\(self.challenge)\",\"expires_in\":300,\"resend_after\":0}")
                case "/v1/auth/sms/verify": return .init(headers: ["Set-Cookie": "\(self.cookie); HttpOnly"])
                case "/v1/auth/logout": return .init()
                default: XCTFail("Unexpected request after invalid response"); return .init(status: 500)
                }
            }
            defer { fixture.close() }
            let auth = try SMSAuth(origin: fixture.origin, configuration: fixture.configuration)
            do {
                _ = try await auth.start(phone: "+15555550123")
                _ = try await auth.verify(code: "123456")
                XCTFail("Invalid response accepted")
            } catch let error as SMSAuthError {
                XCTAssertEqual(error.code, "invalid_response")
                XCTAssertFalse(error.localizedDescription.contains("secret"))
            }
            XCTAssertEqual(reached.last, invalidStage)
            try await auth.cancel()
        }
    }

    func testInvalidPhoneNeverSendsAndServerCooldownIsHonored() async throws {
        var requests = 0
        let fixture = try HTTPFixture { _ in
            requests += 1
            return .init(status: 429, headers: ["Retry-After": "120"], body: "{\"message\":\"\(fixtureKey)\"}")
        }
        defer { fixture.close() }
        let auth = try SMSAuth(origin: fixture.origin, configuration: fixture.configuration)
        do { _ = try await auth.start(phone: "555"); XCTFail("Invalid phone accepted") }
        catch let error as SMSAuthError { XCTAssertEqual(error.code, "invalid_phone") }
        XCTAssertEqual(requests, 0)
        do { _ = try await auth.start(phone: "+15555550123"); XCTFail("Rate limit ignored") }
        catch let error as SMSAuthError {
            XCTAssertEqual(error.code, "rate_limited")
            XCTAssertGreaterThan(try XCTUnwrap(error.retryAt).timeIntervalSinceNow, 110)
            XCTAssertFalse(error.localizedDescription.contains(fixtureKey))
        }
        XCTAssertEqual(requests, 1)
        for origin in ["http://example.com", "https://example.com/path", "https://user:pass@example.com", "https://example.com?token=x"] {
            XCTAssertThrowsError(try SMSAuth(origin: origin))
        }
    }

    func testSMSConsumedOTPAndMintRetriesRetainPrivateSession() async throws {
        var starts = 0, verifies = 0, mints = 0, revokes = 0, logouts = 0
        let fixture = try HTTPFixture { request in
            XCTAssertNotNil(request.headers["origin"]); XCTAssertNil(request.headers["authorization"])
            switch (request.method, request.path) {
            case ("POST", "/v1/auth/sms/start"):
                starts += 1; XCTAssertNil(request.headers["cookie"]); XCTAssertEqual(request.json["phone"] as? String, "+15555550123")
                return .init(body: "{\"challenge_id\":\"\(self.challenge)\",\"expires_in\":300,\"resend_after\":30}")
            case ("POST", "/v1/auth/sms/verify"):
                verifies += 1; XCTAssertNil(request.headers["cookie"])
                if request.json["code"] as? String != "123456" { return .init(status: 400, body: #"{"error":"invalid_or_expired_otp"}"#) }
                return .init(headers: ["Set-Cookie": "\(self.cookie); HttpOnly; Secure; SameSite=Lax", "Content-Type": "application/json"])
            case ("POST", "/v1/api-keys"):
                mints += 1; XCTAssertEqual(request.headers["cookie"], self.cookie); XCTAssertEqual(request.json["label"] as? String, "Nanocodex on Test iPhone")
                if mints == 1 { return .init(status: 503, body: #"{"error":"temporarily_unavailable"}"#) }
                return .init(body: "{\"api_key\":\"\(fixtureKey)\"}")
            case ("DELETE", "/v1/api-keys/abcdefgh1234"): revokes += 1; return .init()
            case ("POST", "/v1/auth/logout"): logouts += 1; XCTAssertEqual(request.headers["cookie"], self.cookie); return .init(status: 503)
            default: XCTFail("Unexpected SMS request"); return .init(status: 404)
            }
        }
        defer { fixture.close() }
        let auth = try SMSAuth(origin: fixture.origin, deviceName: "Test iPhone", configuration: fixture.configuration)
        let challenge = try await auth.start(phone: "+1 (555) 555-0123")
        XCTAssertGreaterThan(challenge.resendAt.timeIntervalSinceNow, 20)
        do { _ = try await auth.start(phone: "+15555550123"); XCTFail("Cooldown ignored") }
        catch let error as SMSAuthError { XCTAssertEqual(error.code, "rate_limited"); XCTAssertNotNil(error.retryAt) }
        do { _ = try await auth.verify(code: "000000"); XCTFail("Wrong code accepted") }
        catch let error as SMSAuthError { XCTAssertEqual(error.code, "invalid_or_expired_otp") }
        do { _ = try await auth.verify(code: "123456"); XCTFail("Failed mint accepted") } catch let error as SMSAuthError { XCTAssertEqual(error.status, 503) }
        async let first = auth.verify(code: "123456")
        async let second = auth.verify(code: "123456")
        let credentials = try await [first, second]
        XCTAssertEqual(credentials[0], credentials[1]); XCTAssertEqual(credentials[0].apiKey, fixtureKey)
        try await auth.complete(); try await auth.cancel()
        XCTAssertEqual(starts, 1); XCTAssertEqual(verifies, 2); XCTAssertEqual(mints, 2); XCTAssertEqual(revokes, 0); XCTAssertEqual(logouts, 1)
        let publicJSON = String(decoding: try JSONEncoder().encode(challenge), as: UTF8.self)
        XCTAssertFalse(publicJSON.contains(fixtureKey)); XCTAssertFalse(publicJSON.contains(cookie)); XCTAssertFalse(publicJSON.contains(self.challenge))
    }

    func testCancelRacingMintRevokesOnlyAttemptKeyAndCanRetryFailedCleanup() async throws {
        let minted = expectation(description: "mint started")
        var deleted: [String] = []
        let fixture = try HTTPFixture { request in
            switch request.path {
            case "/v1/auth/sms/start": return .init(body: "{\"challenge_id\":\"\(self.challenge)\",\"expires_in\":300,\"resend_after\":0}")
            case "/v1/auth/sms/verify": return .init(headers: ["Set-Cookie": "\(self.cookie); HttpOnly"])
            case "/v1/api-keys": minted.fulfill(); return .init(body: "{\"api_key\":\"\(fixtureKey)\"}", delay: 0.2)
            case "/v1/api-keys/abcdefgh1234":
                XCTAssertEqual(request.method, "DELETE"); deleted.append(request.path)
                return .init(status: deleted.count == 1 ? 503 : 200)
            case "/v1/auth/logout": return .init()
            default: XCTFail("Unexpected request"); return .init(status: 404)
            }
        }
        defer { fixture.close() }
        let auth = try SMSAuth(origin: fixture.origin, configuration: fixture.configuration)
        _ = try await auth.start(phone: "+15555550123")
        let verification = Task { try await auth.verify(code: "123456") }
        await fulfillment(of: [minted], timeout: 3)
        do { try await auth.cancel(); XCTFail("Failed revocation hidden") } catch let error as SMSAuthError { XCTAssertEqual(error.status, 503) }
        do { _ = try await verification.value; XCTFail("Cancelled attempt adopted") } catch let error as SMSAuthError { XCTAssertEqual(error.code, "cancelled") }
        try await auth.cancel(); XCTAssertEqual(deleted, ["/v1/api-keys/abcdefgh1234", "/v1/api-keys/abcdefgh1234"])
    }

    func testCompletePreventsCancelFromRevokingPersistedKeyDuringLogout() async throws {
        let loggingOut = expectation(description: "logout started")
        var deleted = 0
        let fixture = try HTTPFixture { request in
            switch request.path {
            case "/v1/auth/sms/start": return .init(body: "{\"challenge_id\":\"\(self.challenge)\",\"expires_in\":300,\"resend_after\":0}")
            case "/v1/auth/sms/verify": return .init(headers: ["Set-Cookie": "\(self.cookie); HttpOnly"])
            case "/v1/api-keys": return .init(body: "{\"api_key\":\"\(fixtureKey)\"}")
            case "/v1/auth/logout": loggingOut.fulfill(); return .init(status: 503, delay: 0.2)
            default: deleted += 1; return .init()
            }
        }
        defer { fixture.close() }
        let auth = try SMSAuth(origin: fixture.origin, configuration: fixture.configuration)
        _ = try await auth.start(phone: "+15555550123"); _ = try await auth.verify(code: "123456")
        let completion = Task { try await auth.complete() }
        await fulfillment(of: [loggingOut], timeout: 3)
        try await auth.cancel(); try await completion.value; XCTAssertEqual(deleted, 0)
    }
}
