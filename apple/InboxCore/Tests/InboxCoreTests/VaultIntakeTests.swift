import Foundation
import XCTest
@testable import InboxCore

final class VaultIntakeTests: XCTestCase {
    func testAutomaticBrowserPresentationRequiresCurrentAgentAndUnexpiredRequest() throws {
        let hint: JSON = .object(["type": .string("browser_vault_takeover"), "status": .string("input_required"),
            "challenge_id": .string(String(repeating: "a", count: 32)), "agent_id": .string("agent_1"),
            "origin": .string("https://example.com"), "expires_at": .number(2000)])
        let intake = try XCTUnwrap(VaultIntake.parse(hint))
        XCTAssertEqual(intake.expiresAt, 2000)
        XCTAssertTrue(intake.isCurrentBrowserRequest(agentID: "agent_1", now: Date(timeIntervalSince1970: 1)))
        XCTAssertFalse(intake.isCurrentBrowserRequest(agentID: "other", now: Date(timeIntervalSince1970: 1)))
        XCTAssertFalse(intake.isCurrentBrowserRequest(agentID: "agent_1", now: Date(timeIntervalSince1970: 2)))
        XCTAssertFalse(intake.isCurrentBrowserRequest(agentID: "agent_1", now: Date(timeIntervalSince1970: 3)))
    }

    func testOnlySupportedHintsAndExactHTTPSOriginsAreAccepted() {
        func hint(_ kind: String, _ origin: String = "") -> JSON {
            .object(["type": .string("vault_intake"), "status": .string("input_required"), "kind": .string(kind), "origin": .string(origin)])
        }
        for kind in ["login", "api_key", "card", "address", "phone"] {
            XCTAssertEqual(VaultIntake.parse(hint(kind))?.kind, kind)
        }
        XCTAssertNil(VaultIntake.parse(hint("ssh")))
        for origin in ["http://example.com", "https://user:pass@example.com", "https://example.com/path", "https://example.com?secret=x", "https://example.com/", "https://example.com:443"] {
            XCTAssertNil(VaultIntake.parse(hint("login", origin)))
        }
        XCTAssertNotNil(VaultIntake.parse(hint("login", "https://example.com")))
        XCTAssertNil(VaultIntake.parse(hint("card", "https://example.com")))
        XCTAssertNil(VaultIntake.parse(.object(["unrelated": hint("login")])))
    }

    func testOnlySuccessfulIntakeToolCanPresentSecureForm() {
        let value: JSON = .object(["type": .string("vault_intake"), "status": .string("input_required"), "kind": .string("login")])
        var ordinary = ToolPresentation(name: "browser_execute", arguments: .null)
        ordinary.finish(value)
        XCTAssertNil(ordinary.vaultIntake)
        var intake = ToolPresentation(name: "request_vault_intake", arguments: .null)
        intake.finish(value)
        XCTAssertNotNil(intake.vaultIntake)
        intake.finish(value, failed: true)
        XCTAssertNil(intake.vaultIntake)
    }

    func testAuthorizationRequiresExistingIDAndOrigin() {
        var value: [String: JSON] = ["type": .string("vault_intake"), "status": .string("input_required"), "kind": .string("login"), "operation": .string("authorize_origin"), "origin": .string("https://example.com")]
        XCTAssertNil(VaultIntake.parse(.object(value)))
        value["vault_id"] = .string(String(repeating: "a", count: 22))
        XCTAssertEqual(VaultIntake.parse(.object(value))?.operation, "authorize_origin")
    }

    func testSecretSubmissionUsesDirectAPIAndReceiptIgnoresServerName() async throws {
        let fixture = try HTTPFixture { request in
            XCTAssertEqual(request.method, "POST")
            XCTAssertEqual(request.path, "/v1/credentials/vault/login")
            XCTAssertEqual(request.headers["authorization"], "Bearer \(fixtureKey)")
            XCTAssertEqual(request.headers["cache-control"], "no-store")
            XCTAssertEqual(request.json["password"] as? String, "secret-fixture")
            return FixtureReply(body: #"{"id":"aaaaaaaaaaaaaaaaaaaaaa","kind":"login","name":"untrusted response"}"#)
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey))
        defer { client.close() }
        let receipt = try await client.saveVaultItem(kind: "login", values: ["name": "Example", "username": "user", "password": "secret-fixture"], configuration: fixture.configuration)
        XCTAssertEqual(receipt.name, "Example")
    }

    func testFailureDiscardsServerBodyAndDoesNotReplay() async throws {
        let fixture = try HTTPFixture { request in
            FixtureReply(status: 500, body: "private-server-error")
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey))
        defer { client.close() }
        do {
            _ = try await client.saveVaultItem(kind: "api_key", values: ["name": "Example", "api_key": "secret"], configuration: fixture.configuration)
            XCTFail("Expected failure")
        } catch {
            XCTAssertFalse(String(describing: error).contains("private-server-error"))
        }
    }
    func testBrowserVerificationHintAndDirectSubmission() async throws {
        var value: [String: JSON] = ["type": .string("vault_intake"), "status": .string("input_required"),
            "operation": .string("browser_verification"), "kind": .string("login"),
            "vault_id": .string(String(repeating: "a", count: 22)), "origin": .string("https://example.com"),
            "challenge_id": .string(String(repeating: "b", count: 22)), "agent_id": .string("agent_1")]
        let intake = try XCTUnwrap(VaultIntake.parse(.object(value)))
        value["code"] = .string("123456")
        XCTAssertNil(VaultIntake.parse(.object(value)))
        let fixture = try HTTPFixture { request in
            XCTAssertEqual(request.method, "POST")
            XCTAssertEqual(request.path, "/v1/agents/agent_1/browser-vault/challenge")
            XCTAssertEqual(request.headers["cache-control"], "no-store")
            XCTAssertEqual(request.json["code"] as? String, "123456")
            XCTAssertEqual(request.json.count, 2)
            return FixtureReply(body: #"{"type":"browser_vault_challenge_receipt","status":"submitted","challenge_id":"bbbbbbbbbbbbbbbbbbbbbb"}"#)
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey))
        defer { client.close() }
        try await client.submitBrowserVerification(intake: intake, code: "123456", configuration: fixture.configuration)
    }

    func testDedicatedChallengeToolPresentation() throws {
        let hint: JSON = .object(["type": .string("browser_vault_challenge"), "status": .string("input_required"),
            "challenge_id": .string(String(repeating: "b", count: 22)), "agent_id": .string("agent_1"),
            "origin": .string("https://example.com"), "expires_at": .number(9999999999999)])
        var tool = ToolPresentation(name: "browser_vault_request_challenge", arguments: .null)
        tool.finish(hint)
        XCTAssertEqual(tool.vaultIntake?.operation, "browser_verification")
        var other = ToolPresentation(name: "browser_execute", arguments: .null)
        other.finish(hint)
        XCTAssertNil(other.vaultIntake)
    }

    func testTakeoverUsesPrivateDirectResponse() async throws {
        let hint: JSON = .object(["type": .string("browser_vault_takeover"), "status": .string("input_required"),
            "challenge_id": .string(String(repeating: "b", count: 22)), "agent_id": .string("agent_1"),
            "origin": .string("https://example.com"), "expires_at": .number(9999999999999)])
        var tool = ToolPresentation(name: "browser_vault_request_takeover", arguments: .null); tool.finish(hint)
        let intake = try XCTUnwrap(tool.vaultIntake)
        XCTAssertEqual(intake.operation, "browser_takeover")
        let fixture = try HTTPFixture { request in
            XCTAssertEqual(request.path, "/v1/agents/agent_1/browser-vault/takeover")
            XCTAssertEqual(request.headers["cache-control"], "no-store")
            XCTAssertEqual(request.json["text"] as? String, "private-text")
            return FixtureReply(body: #"{"status":"active","image":"data:image/png;base64,iVBORw0KGgo=","width":800,"height":600}"#)
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey)); defer { client.close() }
        let frame = try await client.browserTakeover(intake: intake, action: ["action": .string("type"), "text": .string("private-text")], configuration: fixture.configuration)
        guard case .active(let data, let width, _) = frame else { return XCTFail("Expected private frame") }
        XCTAssertEqual(width, 800); XCTAssertEqual(data.count, 8)
    }

    func testTakeoverInputMetadataAndFinishBoundary() throws {
        let hint: JSON = .object(["type": .string("email"), "multiline": .bool(false)])
        let region: JSON = .object(["x": .number(0.1), "y": .number(0.2),
            "width": .number(0.5), "height": .number(0.1), "type": .string("password"), "multiline": .bool(false)])
        var fields: [String: JSON] = ["status": .string("active"),
            "image": .string("data:image/png;base64,iVBORw0KGgo="), "width": .number(800), "height": .number(600),
            "keyboard": hint, "inputs": .array([region])]
        guard case .activeWithInput(_, let width, _, let keyboard, let inputs) = try BrowserTakeoverFrame.parse(.object(fields)) else {
            return XCTFail("Expected input metadata")
        }
        XCTAssertEqual(width, 800)
        XCTAssertEqual(keyboard?.type, "email")
        XCTAssertEqual(inputs.first?.keyboard.type, "password")
        XCTAssertThrowsError(try BrowserTakeoverFrame.parse(.object(fields), finishing: true))
        let finished: JSON = .object(["status": .string("finished")])
        XCTAssertThrowsError(try BrowserTakeoverFrame.parse(finished))
        guard case .finished = try BrowserTakeoverFrame.parse(finished, finishing: true) else {
            return XCTFail("Expected confirmed finish")
        }
        XCTAssertThrowsError(try BrowserTakeoverFrame.parse(.object(["status": .string("finished"), "extra": .bool(true)]), finishing: true))
        fields["secret"] = .string("unexpected")
        XCTAssertThrowsError(try BrowserTakeoverFrame.parse(.object(fields)))
    }

    func testTakeoverRejectsMalformedMetadata() {
        let base: [String: JSON] = ["status": .string("active"),
            "image": .string("data:image/png;base64,iVBORw0KGgo="), "width": .number(800), "height": .number(600)]
        let invalid: [(String, JSON)] = [
            ("width", .number(.infinity)), ("height", .number(1.5)), ("width", .string("800")),
            ("image", .string("data:image/png;base64,aGVsbG8=")),
            ("keyboard", .null),
            ("keyboard", .object(["type": .string("unknown"), "multiline": .bool(false)])),
            ("keyboard", .object(["type": .string("text"), "multiline": .bool(false), "value": .string("private")])),
            ("inputs", .array(Array(repeating: .null, count: 33))),
            ("inputs", .array([.object(["x": .number(0.9), "y": .number(0), "width": .number(0.2),
                "height": .number(0.1), "type": .string("text"), "multiline": .bool(false)])]))
        ]
        for (key, value) in invalid {
            var fields = base; fields[key] = value
            XCTAssertThrowsError(try BrowserTakeoverFrame.parse(.object(fields)), "Accepted malformed \(key)")
        }
    }

}
