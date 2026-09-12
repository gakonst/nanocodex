import XCTest
@testable import InboxCore

final class TranscriptMediaTests: XCTestCase {
    func testFlattenedAttachmentDescriptorsKeepSurroundingUserText() throws {
        let image = try MessageAttachment(name: "Photo.png", mediaType: "image/png", byteCount: 42)
        let imageText = ImageAttachmentContent.original(image)[0]["text"].string
        let videoText = VideoAttachmentContent.original(id: UUID().uuidString, name: "Clip.mp4", duration: 3,
            mediaType: "video/mp4", byteCount: 123, hasAudio: true)[0]["text"].string
        let result = TranscriptInput(.string("Before\n\n" + imageText + "\n\nBetween\n\n" + videoText + "\n\nAfter"))
        XCTAssertEqual(result.text, "Before\nBetween\nAfter")
        XCTAssertEqual(result.imageFiles, [image])
        XCTAssertEqual(result.videos.map(\.name), ["Clip.mp4"])
        XCTAssertTrue(result.images.isEmpty)
    }

    func testResponsesContentAliasesKeepVideoFramesInsideOneAttachment() {
        let parts = VideoAttachmentContent.make(id: UUID().uuidString, name: "Clip.mp4", duration: 2,
            timestamps: [0, 1], images: [Data([1]), Data([2])]).map { part -> JSON in
            if part["type"].string == "text" { return .object(["type": .string("input_text"), "text": part["text"]]) }
            return .object(["type": .string("input_image"), "image_url": .object(["url": part["image_url"]])])
        }
        let result = TranscriptInput(.array(parts + [.object(["type": .string("input_text"), "text": .string("My caption")])]))
        XCTAssertEqual(result.text, "My caption")
        XCTAssertTrue(result.images.isEmpty)
        XCTAssertEqual(result.videos.count, 1)
        XCTAssertEqual(result.videos.first?.images.count, 2)
    }

    func testMalformedAttachmentDescriptionRemainsUserText() {
        let text = "Explain this format:\n" + ImageAttachmentContent.prefix + "not JSON"
        XCTAssertEqual(TranscriptInput(.string(text)).text, text)
    }

    func testInspectionResultsStayAvailableAsDiagnosticsWithoutBecomingDeliverables() {
        var inspected = ToolPresentation(name: "functions.exec", arguments: .string("image((await tools.view_image({path: frame})).image_url)"))
        inspected.finish(.object(["image_url": .string("data:image/png;base64,AQ==")]))
        XCTAssertEqual(inspected.generatedIsInspection, true)
        XCTAssertNotNil(inspected.generatedResults, "Inspection evidence stays in activity")
        inspected.generatedIsInspection = nil
        XCTAssertTrue(inspected.isInspectionOutput, "Old cached transcripts must also keep inspected frames out of replies")
        XCTAssertFalse(ToolOutputVisibility.isInspection(name: "functions.exec", arguments: "generatedImage(await tools.imagegen({prompt: description}))"))
        XCTAssertFalse(ToolOutputVisibility.isInspection(name: "make_chart", arguments: ""))
    }

    func testOriginalPreviewDownloadsAuthenticatedBytesAndRejectsTruncation() async throws {
        let original = "original image bytes"
        let attachment = try MessageAttachment(name: "Photo.png", mediaType: "image/png", byteCount: original.utf8.count)
        let fixture = try HTTPFixture { request in
            XCTAssertEqual(request.headers["authorization"], "Bearer " + fixtureKey)
            XCTAssertEqual(request.headers["accept"], "image/png")
            XCTAssertTrue(request.path.hasSuffix("/attachments/" + attachment.id.lowercased()))
            return FixtureReply(headers: ["Content-Type": "image/png"], body: original)
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try .init(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        let file = try await client.downloadAttachment(agentID: "media-test", attachment: attachment)
        defer { try? FileManager.default.removeItem(at: file) }
        XCTAssertTrue(file.isFileURL)
        XCTAssertEqual(file.pathExtension, "png")
        XCTAssertEqual(try Data(contentsOf: file), Data(original.utf8))
        let truncated = try MessageAttachment(id: attachment.id, name: "Photo.png", mediaType: "image/png", byteCount: original.utf8.count + 1)
        do {
            let unexpected = try await client.downloadAttachment(agentID: "media-test", attachment: truncated)
            try? FileManager.default.removeItem(at: unexpected)
            XCTFail("Truncated originals must not reach the previewer")
        } catch { XCTAssertEqual(error as? APIError, .invalidResponse) }
    }
}
