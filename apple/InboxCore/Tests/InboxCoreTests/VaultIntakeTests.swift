import Foundation
import XCTest
@testable import InboxCore

final class VaultIntakeTests: XCTestCase {
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
}
