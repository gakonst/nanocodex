import XCTest
import ImageIO
import CoreGraphics
import CryptoKit
@testable import InboxCore

final class MessageAttachmentTests: XCTestCase {
    func testImagePreparationPreservesOriginalAndCreatesAnOrientedPreview() throws {
        let original = try png(width: 2200, height: 1100, orientation: 6)
        let prepared = try AttachmentPreparation.prepare(data: original, name: "Camera.png", mediaType: "image/png")
        XCTAssertEqual(prepared.attachment.mediaType, "image/png")
        XCTAssertEqual(prepared.attachment.byteCount, original.count)
        guard case .data(let retained) = prepared.source else { return XCTFail("Expected original data") }
        XCTAssertEqual(retained, original)
        XCTAssertEqual(prepared.content[0]["type"].string, "text")
        XCTAssertTrue(prepared.content[0]["text"].string.contains(prepared.attachment.originalPath))
        XCTAssertFalse(prepared.content[0]["text"].string.contains("base64"))
        let source = try XCTUnwrap(CGImageSourceCreateWithData(prepared.preview as CFData, nil))
        let image = try XCTUnwrap(CGImageSourceCreateImageAtIndex(source, 0, nil))
        XCTAssertEqual(image.width, 320)
        XCTAssertEqual(image.height, 640, "Only the preview is resized and oriented")
        let originalSource = try XCTUnwrap(CGImageSourceCreateWithData(retained as CFData, nil))
        let properties = try XCTUnwrap(CGImageSourceCopyPropertiesAtIndex(originalSource, 0, nil) as? [CFString: Any])
        XCTAssertEqual(properties[kCGImagePropertyPixelWidth] as? Int, 2200)
        XCTAssertEqual(properties[kCGImagePropertyPixelHeight] as? Int, 1100)
    }

    func testLargeOriginalFileIsPreservedWithoutInlineHistoryBytes() throws {
        var bytes = try png()
        bytes.append(Data(repeating: 7, count: 25 * 1024 * 1024 + 1))
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let source = root.appendingPathComponent("large.png")
        try bytes.write(to: source)
        let prepared = try AttachmentPreparation.prepare(url: source)
        let store = try AttachmentStore(scope: "account", rootDirectory: root)
        try store.save(prepared)
        XCTAssertEqual(try Data(contentsOf: store.url(for: prepared.attachment), options: .mappedIfSafe), bytes)
        XCTAssertLessThan(try JSONEncoder().encode(prepared.content).count, 1024)
        let event = try AgentEvent(.object(["cursor": .string("1"), "type": .string("turn_accepted"), "turn_id": .string("image-turn"),
            "input": .array([.object(["type": .string("text"), "text": .string("Read this")])] + prepared.content)]))
        let row = try XCTUnwrap(transcript([event]).first)
        XCTAssertEqual(row.text, "Read this")
        XCTAssertEqual(row.imageFiles, [prepared.attachment])
        XCTAssertNil(row.images)
    }

    func testUnsupportedSourcesAndUnsafeReferencesAreRejected() throws {
        XCTAssertThrowsError(try AttachmentPreparation.prepare(data: Data("not an image".utf8), name: "file.txt", mediaType: "text/plain"))
        XCTAssertThrowsError(try AttachmentPreparation.prepare(data: Data([0, 1, 2]), name: "bad.jpg", mediaType: "image/jpeg"))
        for id in ["../outside", "", "not-a-uuid", "123456781234123412341234567890ab"] {
            XCTAssertThrowsError(try MessageAttachment(id: id, name: "photo.jpg", byteCount: 1))
        }
        XCTAssertThrowsError(try MessageAttachment(name: "../photo.jpg", byteCount: 1))
        XCTAssertNoThrow(try MessageAttachment(name: "photo.jpg", byteCount: 160 * 1024 + 1))
        let forged = Data(#"{"id":"../outside","name":"photo.jpg","mediaType":"image/jpeg","byteCount":1}"#.utf8)
        XCTAssertThrowsError(try JSONDecoder().decode(MessageAttachment.self, from: forged))
    }

    func testStorePersistsOriginalAndPreviewAndKeepsAccountsIsolated() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let store = try AttachmentStore(scope: "account-one", rootDirectory: root)
        let other = try AttachmentStore(scope: "account-two", rootDirectory: root)
        let prepared = try AttachmentPreparation.prepare(data: png(), name: "photo.png", mediaType: "image/png")
        try store.save(prepared)
        let url = try store.url(for: prepared.attachment)
        XCTAssertEqual(url.lastPathComponent, prepared.attachment.id.lowercased() + ".original")
        XCTAssertEqual(try Data(contentsOf: url).count, prepared.attachment.byteCount)
        let restored = try AttachmentStore(scope: "account-one", rootDirectory: root)
        XCTAssertEqual(try restored.content(for: [prepared.attachment]), prepared.content)
        XCTAssertThrowsError(try other.content(for: [prepared.attachment]))
        let preserved = url.deletingLastPathComponent().appendingPathComponent("unrelated.txt")
        try Data("keep".utf8).write(to: preserved)
        try store.prune(keeping: [prepared.attachment.id])
        XCTAssertTrue(FileManager.default.fileExists(atPath: url.path))
        try store.prune(keeping: [])
        XCTAssertFalse(FileManager.default.fileExists(atPath: url.path))
        XCTAssertTrue(FileManager.default.fileExists(atPath: preserved.path))
        try store.save(prepared)
        try store.remove(prepared.attachment)
        try store.remove(prepared.attachment)
        XCTAssertThrowsError(try store.content(for: [prepared.attachment]))
        for scope in ["", "../account-two", "/tmp", "account/one", "account\\one"] {
            XCTAssertThrowsError(try AttachmentStore(scope: scope, rootDirectory: root))
        }
    }

    func testUploadUsesServiceSelectedPartSizeAndFileBackedBodies() async throws {
        let partSize = 16 * 1024 * 1024 + 3
        var bytes = Data(repeating: 31, count: partSize)
        bytes.append(contentsOf: [1, 2, 3, 4, 5])
        let firstDigest = SHA256.hash(data: bytes.prefix(partSize))
        let attachment = try MessageAttachment(name: "large.png", mediaType: "image/png", byteCount: bytes.count)
        let source = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try bytes.write(to: source)
        defer { try? FileManager.default.removeItem(at: source) }
        let parts = expectation(description: "Both file-backed parts uploaded")
        parts.expectedFulfillmentCount = 2
        let fixture = try HTTPFixture { request in
            if request.path.hasSuffix("/parts/1") {
                XCTAssertEqual(request.body.count, partSize)
                XCTAssertEqual(SHA256.hash(data: request.body), firstDigest)
                parts.fulfill()
                return FixtureReply()
            }
            if request.path.hasSuffix("/parts/2") {
                XCTAssertEqual(request.body, Data([1, 2, 3, 4, 5]))
                parts.fulfill()
                return FixtureReply()
            }
            let complete = request.path.hasSuffix("/complete")
            return FixtureReply(body: "{\"path\":\"\(attachment.originalPath)\",\"size\":\(attachment.byteCount),\"part_size\":\(partSize),\"next_part\":1,\"complete\":\(complete)}")
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try AccountCredential(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        let path = try await client.uploadAttachment(agentID: "attachment-adaptive", attachment: attachment, source: source)
        XCTAssertEqual(path, attachment.originalPath)
        await fulfillment(of: [parts], timeout: 1)
    }

    func testExistingJPEGDraftsMigrateOnceWithoutChangingTheirBytes() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        let directory = root.appendingPathComponent("account")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let jpeg = try AttachmentPreparation.prepare(data: png(), name: "legacy.png", mediaType: "image/png").preview
        let old = try MessageAttachment(name: "old.jpg", byteCount: jpeg.count)
        let videoID = UUID().uuidString.lowercased()
        try jpeg.write(to: directory.appendingPathComponent(old.id.lowercased() + ".jpg"))
        try jpeg.write(to: directory.appendingPathComponent(videoID + ".jpg"))
        try Data([1]).write(to: directory.appendingPathComponent(videoID + ".mp4"))
        let migrated = try AttachmentStore(scope: "account", rootDirectory: root)
        XCTAssertEqual(try Data(contentsOf: migrated.url(for: old)), jpeg)
        XCTAssertEqual(try migrated.content(for: [old]), try old.originalContent(path: old.originalPath))
        XCTAssertFalse(FileManager.default.fileExists(atPath: directory.appendingPathComponent(videoID + ".original").path))
        let original = try migrated.url(for: old)
        try FileManager.default.removeItem(at: original)
        _ = try AttachmentStore(scope: "account", rootDirectory: root)
        XCTAssertFalse(FileManager.default.fileExists(atPath: original.path), "Reopening must not rerun migration or turn missing originals into previews")
    }

    func testStoreRejectsCorruptBytesAndLinks() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let store = try AttachmentStore(scope: "account", rootDirectory: root)
        let prepared = try AttachmentPreparation.prepare(data: png(), name: "photo.png", mediaType: "image/png")
        try store.save(prepared)
        let url = try store.url(for: prepared.attachment)
        try Data(repeating: 0, count: prepared.attachment.byteCount).write(to: url)
        XCTAssertThrowsError(try store.content(for: [prepared.attachment]))
        XCTAssertThrowsError(try store.save(PreparedAttachment(attachment: prepared.attachment, source: prepared.source, preview: Data([0, 1, 2]))))
        try FileManager.default.removeItem(at: url)
        let outside = root.appendingPathComponent("outside.jpg")
        try Data("preserve".utf8).write(to: outside)
        try FileManager.default.createSymbolicLink(at: url, withDestinationURL: outside)
        XCTAssertThrowsError(try store.content(for: [prepared.attachment]))
        XCTAssertThrowsError(try store.save(prepared))
        XCTAssertThrowsError(try store.remove(prepared.attachment))
        XCTAssertEqual(try String(contentsOf: outside, encoding: .utf8), "preserve")
    }

    private func png(width: Int = 48, height: Int = 32, orientation: Int = 1) throws -> Data {
        let context = try XCTUnwrap(CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: width * 4,
                                             space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue))
        context.setFillColor(CGColor(red: 0.2, green: 0.5, blue: 0.8, alpha: 0.5))
        context.fill(CGRect(x: 0, y: 0, width: width, height: height))
        let image = try XCTUnwrap(context.makeImage())
        let data = NSMutableData()
        let destination = try XCTUnwrap(CGImageDestinationCreateWithData(data, "public.png" as CFString, 1, nil))
        CGImageDestinationAddImage(destination, image, [kCGImagePropertyOrientation: orientation] as CFDictionary)
        XCTAssertTrue(CGImageDestinationFinalize(destination))
        return data as Data
    }
}
