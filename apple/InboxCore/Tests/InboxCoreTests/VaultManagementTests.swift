import Foundation
import XCTest
@testable import InboxCore

final class VaultManagementTests: XCTestCase {
    func testPublicCredentialHTTPJourneys() async throws {
        let fixture = try HTTPFixture { request in
            if request.method == "DELETE" {
                XCTAssertEqual(request.path, "/v1/credentials/chatgpt")
                return FixtureReply(status: 204, body: "")
            }
            XCTAssertEqual(request.path, "/v1/credentials/chatgpt/login")
            if request.method == "POST" {
                return FixtureReply(body: #"{"state":"pending","verification_url":"https://auth.openai.com/codex/device","user_code":"TEST-CODE","expires_at":1800000000000,"poll_after_ms":1000}"#)
            }
            if request.query == nil { return FixtureReply(body: #"{"state":"authenticated","account_id":"synthetic"}"#) }
            return FixtureReply(body: #"{"state":"expired"}"#)
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey))
        defer { client.close() }
        let started = try ChatGPTLoginReceipt(await client.nativeCredentialOperation(path: "/v1/credentials/chatgpt/login", method: "POST", configuration: fixture.configuration))
        XCTAssertEqual(started.userCode, "TEST-CODE")
        let polled = try ChatGPTLoginReceipt(await client.nativeCredentialOperation(path: "/v1/credentials/chatgpt/login", configuration: fixture.configuration))
        XCTAssertEqual(polled.state, "authenticated")
        XCTAssertNil(polled.userCode)
        let deleted = try await client.nativeCredentialOperation(path: "/v1/credentials/chatgpt", method: "DELETE", configuration: fixture.configuration)
        XCTAssertEqual(deleted, .null)
    }
    func testNumericProviderReceiptOverPublicHTTP() async throws {
        let fixture = try HTTPFixture { request in
            XCTAssertEqual(request.path, "/v1/vault/card")
            XCTAssertEqual(request.json["operation"] as? String, "balance")
            return FixtureReply(body: #"{"status":"ready","balance":12.5,"currency":"USD","observed_at":1791244800000}"#)
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey))
        defer { client.close() }
        let receipt = try ProviderCardReceipt(await client.providerCard(operation: "balance", vaultID: String(repeating: "a", count: 22), configuration: fixture.configuration))
        XCTAssertTrue(receipt.display.contains("USD 12.50"))
        XCTAssertTrue(receipt.display.contains("Observed"))
    }
    func testPrivateNativeSaveRetainsOperationAndOwnerHeaders() async throws {
        let operation = UUID()
        let fixture = try HTTPFixture { request in
            XCTAssertEqual(request.path, "/v1/credentials/vault/api_key")
            XCTAssertEqual(request.headers["authorization"], "Bearer \(fixtureKey)")
            XCTAssertEqual(request.headers["x-nanocodex-operation-id"], operation.uuidString)
            XCTAssertEqual(request.json["api_key"] as? String, "synthetic-private-value")
            return FixtureReply(body: #"{"id":"aaaaaaaaaaaaaaaaaaaaaa","kind":"api_key","secret":"must-not-be-returned"}"#)
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey))
        defer { client.close() }
        for _ in 0..<2 {
            let receipt = try await client.saveVaultItem(kind: "api_key", values: ["name":"Synthetic", "api_key":"synthetic-private-value"], configuration: fixture.configuration, operationID: operation)
            XCTAssertEqual(receipt.name, "Synthetic")
            XCTAssertEqual(receipt.id, String(repeating: "a", count: 22))
        }
    }

}
