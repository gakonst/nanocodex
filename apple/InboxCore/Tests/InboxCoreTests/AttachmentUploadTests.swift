import XCTest
@testable import InboxCore

final class AttachmentUploadTests: XCTestCase {
    func testCancelledUploadDoesNotCreateRemoteState() async throws {
        let fixture = try HTTPFixture { _ in
            XCTFail("A cancelled upload must not send a request")
            return FixtureReply()
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        let attachment = try MessageAttachment(name: "photo.png", mediaType: "image/png", byteCount: 1)
        do {
            _ = try await client.uploadAttachment(agentID: "cancelled", attachment: attachment,
                source: URL(fileURLWithPath: "/missing-cancelled-attachment"), isCancelled: { true })
            XCTFail("Expected cancellation")
        } catch is CancellationError {}
    }

    func testCancellationAfterOriginalReceiptSkipsPreview() async throws {
        // Both a completed retry and a newly completed upload must check the
        // outbox cancellation/account generation before sending its preview.
        for alreadyComplete in [true, false] {
            let source = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
            try Data([1, 2, 3]).write(to: source)
            defer { try? FileManager.default.removeItem(at: source) }
            let attachment = try MessageAttachment(name: "photo.png", mediaType: "image/png", byteCount: 3)
            let state = UploadFixtureState()
            let fixture = try HTTPFixture { request in
                XCTAssertFalse(request.path.hasSuffix("/preview"), "Cancelled messages must not upload previews")
                let complete = alreadyComplete || request.path.hasSuffix("/complete")
                if complete { state.cancel() }
                return FixtureReply(body: "{\"path\":\"\(attachment.originalPath)\",\"size\":3,\"part_size\":3,\"next_part\":1,\"complete\":\(complete)}")
            }
            defer { fixture.close() }
            let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
            defer { client.close() }
            do {
                _ = try await client.uploadAttachment(agentID: "cancelled", attachment: attachment, source: source, preview: source,
                                                       isCancelled: { state.isCancelled })
                XCTFail("Expected cancellation")
            } catch is CancellationError {}
        }
    }

    func testPreviewFailurePreservesStatusAndRetryReusesCompletedOriginal() async throws {
        let source = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        let bytes = Data([1, 2, 3])
        try bytes.write(to: source)
        defer { try? FileManager.default.removeItem(at: source) }
        let attachment = try MessageAttachment(name: "photo.png", mediaType: "image/png", byteCount: bytes.count)
        let state = UploadFixtureState()
        let part = expectation(description: "Original uploaded only once")
        part.assertForOverFulfill = true
        let previews = expectation(description: "Failed preview retried")
        previews.expectedFulfillmentCount = 2
        let fixture = try HTTPFixture { request in
            XCTAssertTrue(request.path.contains(attachment.id.lowercased()), "Retry retains the attachment identity")
            if request.path.hasSuffix("/parts/1") {
                XCTAssertEqual(request.body, bytes)
                part.fulfill()
                return FixtureReply()
            }
            if request.path.hasSuffix("/preview") {
                XCTAssertEqual(request.body, bytes)
                XCTAssertEqual(request.headers["content-type"], "image/jpeg")
                previews.fulfill()
                return FixtureReply(status: state.nextPreview() == 1 ? 503 : 200)
            }
            if request.path.hasSuffix("/complete") { state.complete() }
            return FixtureReply(body: "{\"path\":\"\(attachment.originalPath)\",\"size\":3,\"part_size\":3,\"next_part\":1,\"complete\":\(state.isComplete)}")
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        do {
            _ = try await client.uploadAttachment(agentID: "preview-retry", attachment: attachment, source: source, preview: source)
            XCTFail("The preview failure must keep the message in the outbox")
        } catch {
            XCTAssertEqual(error as? APIError, .http(503), "Do not misreport a server failure as an unreadable response")
        }
        let path = try await client.uploadAttachment(agentID: "preview-retry", attachment: attachment, source: source, preview: source)
        XCTAssertEqual(path, attachment.originalPath)
        XCTAssertEqual(TranscriptInput(.array(try attachment.originalContent(path: path))).imageFiles, [attachment])
        await fulfillment(of: [part, previews], timeout: 1)
    }
}

private final class UploadFixtureState: @unchecked Sendable {
    private let lock = NSLock()
    private var cancelled = false
    private var completed = false
    private var previews = 0
    var isCancelled: Bool { lock.lock(); defer { lock.unlock() }; return cancelled }
    var isComplete: Bool { lock.lock(); defer { lock.unlock() }; return completed }
    func cancel() { lock.lock(); defer { lock.unlock() }; cancelled = true }
    func complete() { lock.lock(); defer { lock.unlock() }; completed = true }
    func nextPreview() -> Int { lock.lock(); defer { lock.unlock() }; previews += 1; return previews }
}
