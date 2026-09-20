import Foundation
import CryptoKit
import XCTest
@testable import InboxCore

final class DirectAttachmentTests: XCTestCase {
    func testResumeSkipsAcknowledgedPartsAndPreviewUploadsDirectly() async throws {
        let bytes = Data([1, 2, 3, 4])
        let source = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try bytes.write(to: source)
        defer { try? FileManager.default.removeItem(at: source) }
        let attachment = try MessageAttachment(name: "a.png", mediaType: "image/png", byteCount: bytes.count)
        let uploaded = expectation(description: "preview uploaded")
        let r2 = try HTTPFixture(host: UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased() + ".r2.cloudflarestorage.com") { request in
            XCTAssertEqual(request.path, "/preview")
            XCTAssertEqual(request.method, "PUT")
            XCTAssertNil(request.headers["authorization"])
            XCTAssertNil(request.headers["cookie"])
            XCTAssertEqual(request.body, bytes)
            XCTAssertEqual(request.headers["content-md5"], Data(Insecure.MD5.hash(data: bytes)).base64EncodedString())
            uploaded.fulfill()
            return FixtureReply(headers: ["ETag": "\"preview-etag\""])
        }
        defer { r2.close() }
        let fixture = try HTTPFixture { request in
            XCTAssertEqual(request.method, "POST")
            XCTAssertEqual(request.headers["authorization"], "Bearer " + fixtureKey)
            if request.path.hasSuffix("/preview/complete") {
                XCTAssertEqual(request.json["etag"] as? String, "\"preview-etag\"")
                return FixtureReply(body: "{\"complete\":true}")
            }
            if request.path.hasSuffix("/preview") {
                XCTAssertEqual(request.json["size"] as? Int, 4)
                let md5 = request.json["md5"] as! String
                return FixtureReply(body: "{\"url\":\"\(r2.origin)/preview\",\"headers\":{\"content-length\":\"4\",\"content-md5\":\"\(md5)\"},\"expires_at\":4102444800000}")
            }
            if request.path.contains("/parts/") {
                XCTAssertTrue(request.path.hasSuffix("/parts/2"))
                XCTAssertEqual(request.json["size"] as? Int, 2)
                XCTAssertEqual(request.json["md5"] as? String, Data(Insecure.MD5.hash(data: Data([3, 4]))).base64EncodedString())
                return FixtureReply(body: "{\"complete\":true,\"part\":2}")
            }
            return FixtureReply(body: "{\"transport\":\"r2\",\"path\":\"\(attachment.originalPath)\",\"size\":4,\"part_size\":2,\"next_part\":2,\"complete\":\(request.path.hasSuffix("/complete"))}")
        }
        defer { fixture.close() }
        fixture.configuration.httpAdditionalHeaders = ["Cookie": "account-cookie"]
        let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        let path = try await client.uploadAttachment(agentID: "direct-test", attachment: attachment, source: source, preview: source)
        XCTAssertEqual(path, attachment.originalPath)
        await fulfillment(of: [uploaded], timeout: 1)
    }

    func testRejectedDirectUploadNeverFallsBackOrRetries() async throws {
        let source = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try Data([1]).write(to: source)
        defer { try? FileManager.default.removeItem(at: source) }
        let attachment = try MessageAttachment(name: "a.png", mediaType: "image/png", byteCount: 1)
        let attempted = expectation(description: "one expired R2 request")
        attempted.assertForOverFulfill = true
        let r2 = try HTTPFixture(host: UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased() + ".r2.cloudflarestorage.com") { request in
            XCTAssertNil(request.headers["authorization"])
            attempted.fulfill()
            return FixtureReply(status: 403)
        }
        defer { r2.close() }
        let fixture = try HTTPFixture { request in
            XCTAssertEqual(request.method, "POST")
            XCTAssertFalse(request.path.hasSuffix("/complete"))
            if request.path.hasSuffix("/parts/1") {
                let md5 = request.json["md5"] as! String
                return FixtureReply(body: "{\"url\":\"\(r2.origin)/part?secret-capability\",\"headers\":{\"content-length\":\"1\",\"content-md5\":\"\(md5)\"},\"expires_at\":4102444800000}")
            }
            return FixtureReply(body: "{\"transport\":\"r2\",\"path\":\"\(attachment.originalPath)\",\"size\":1,\"part_size\":1,\"next_part\":1,\"complete\":false}")
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        do {
            _ = try await client.uploadAttachment(agentID: "direct-test", attachment: attachment, source: source)
            XCTFail("Expected R2 rejection")
        } catch {
            XCTAssertEqual(error as? APIError, .http(403))
            XCTAssertFalse(String(describing: error).contains("secret-capability"))
        }
        await fulfillment(of: [attempted], timeout: 1)
    }

    func testExpiredOrMismatchedReceiptsAndMissingETagFailClosed() async throws {
        for scenario in ["expired", "size", "md5", "etag"] {
            let source = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
            try Data([1]).write(to: source)
            defer { try? FileManager.default.removeItem(at: source) }
            let attachment = try MessageAttachment(name: "a.png", mediaType: "image/png", byteCount: 1)
            let r2 = try HTTPFixture(host: UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased() + ".r2.cloudflarestorage.com") { request in
                XCTAssertEqual(scenario, "etag", "Invalid receipts must not upload")
                XCTAssertNil(request.headers["authorization"])
                return FixtureReply()
            }
            defer { r2.close() }
            let fixture = try HTTPFixture { request in
                XCTAssertEqual(request.method, "POST")
                XCTAssertFalse(request.path.hasSuffix("/complete"))
                if request.path.hasSuffix("/parts/1") {
                    let md5 = scenario == "md5" ? "bad" : request.json["md5"] as! String
                    let size = scenario == "size" ? "2" : "1"
                    let expiry = scenario == "expired" ? 1 : 4102444800000
                    return FixtureReply(body: "{\"url\":\"\(r2.origin)/part\",\"headers\":{\"content-length\":\"\(size)\",\"content-md5\":\"\(md5)\"},\"expires_at\":\(expiry)}")
                }
                return FixtureReply(body: "{\"transport\":\"r2\",\"path\":\"\(attachment.originalPath)\",\"size\":1,\"part_size\":1,\"next_part\":1,\"complete\":false}")
            }
            defer { fixture.close() }
            let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
            defer { client.close() }
            do {
                _ = try await client.uploadAttachment(agentID: "direct-test", attachment: attachment, source: source)
                XCTFail("Expected invalid receipt or response")
            } catch { XCTAssertEqual(error as? APIError, .invalidResponse) }
        }
    }

    func testLegacyReceiptAndUnsafeSignedURLsAreRejected() async throws {
        for signedURL in [nil, "https://evil.invalid/part", "http://" + String(repeating: "a", count: 32) + ".r2.cloudflarestorage.com/part", "https://user@" + String(repeating: "a", count: 32) + ".r2.cloudflarestorage.com/part"] as [String?] {
            let source = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
            try Data([1]).write(to: source)
            defer { try? FileManager.default.removeItem(at: source) }
            let attachment = try MessageAttachment(name: "a.png", mediaType: "image/png", byteCount: 1)
            let fixture = try HTTPFixture { request in
                XCTAssertEqual(request.method, "POST")
                if request.path.hasSuffix("/parts/1") {
                    let md5 = request.json["md5"] as! String
                    return FixtureReply(body: "{\"url\":\"\(signedURL!)\",\"headers\":{\"content-length\":\"1\",\"content-md5\":\"\(md5)\"},\"expires_at\":4102444800000}")
                }
                let transport = signedURL == nil ? "worker" : "r2"
                return FixtureReply(body: "{\"transport\":\"\(transport)\",\"path\":\"\(attachment.originalPath)\",\"size\":1,\"part_size\":1,\"next_part\":1,\"complete\":false}")
            }
            defer { fixture.close() }
            let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
            defer { client.close() }
            do {
                _ = try await client.uploadAttachment(agentID: "direct-test", attachment: attachment, source: source)
                XCTFail("Expected invalid receipt")
            } catch { XCTAssertEqual(error as? APIError, .invalidResponse) }
        }
    }
}
