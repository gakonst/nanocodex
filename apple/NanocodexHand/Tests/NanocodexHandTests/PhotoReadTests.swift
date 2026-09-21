import XCTest
import CoreGraphics
import ImageIO
import InboxCore
@testable import NanocodexHand
#if os(iOS)
import UIKit
#endif

final class PhotoReadTests: XCTestCase {
    private func jpeg(width: Int = 8, height: Int = 6) throws -> Data {
        let context = try XCTUnwrap(CGContext(data: nil, width: width, height: height, bitsPerComponent: 8,
            bytesPerRow: width * 4, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue))
        context.setFillColor(CGColor(red: 1, green: 0, blue: 0, alpha: 1))
        context.fill(CGRect(x: 0, y: 0, width: width, height: height))
        let bytes = NSMutableData()
        let destination = try XCTUnwrap(CGImageDestinationCreateWithData(bytes, "public.jpeg" as CFString, 1, nil))
        CGImageDestinationAddImage(destination, try XCTUnwrap(context.makeImage()), nil)
        XCTAssertTrue(CGImageDestinationFinalize(destination))
        return bytes as Data
    }

    private func directory() throws -> URL {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("photo-read-test-" + UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        addTeardownBlock { try FileManager.default.removeItem(at: root) }
        return root
    }

    @MainActor
    func testPhotoProducesSavedJPEGAndInspectableHostedImageWithoutUpload() async throws {
        let root = try directory(), data = try jpeg()
        let hand = try HandWorkspace(id: "phone-photo", name: "iPhone", root: root)
        let result = try await hand.savePhoto(data, id: "synthetic-photo-id")
        let path = result["path"].string
        XCTAssertTrue(path.hasPrefix("/workspace/photos/"))
        let saved = root.appendingPathComponent(String(path.dropFirst("/workspace/".count)))
        XCTAssertEqual(try Data(contentsOf: saved), data)
        XCTAssertEqual(result["content"].array.count, 2)
        let mcp = result["content"].array[1]
        XCTAssertEqual(mcp["type"].string, "image")
        XCTAssertEqual(mcp["mimeType"].string, "image/jpeg")
        XCTAssertEqual(Data(base64Encoded: mcp["data"].string), data)
        let wire = try HandSession.toolOutput(result, success: true, name: "read_photo")
        XCTAssertEqual(wire["output"].array[0]["type"].string, "input_text")
        XCTAssertEqual(wire["output"].array[1]["type"].string, "input_image")
        XCTAssertEqual(wire["output"].array[1]["image_url"].string, "data:image/jpeg;base64," + data.base64EncodedString())
        XCTAssertEqual(wire["output"].array[1]["detail"].string, "high")
        XCTAssertEqual(wire["structured_result"], result)
        XCTAssertTrue(wire["success"].bool)
        // Ordinary files retain their text contract, even if their content resembles MCP.
        let textWire = try HandSession.toolOutput(result, success: true, name: "read_file")
        XCTAssertFalse(textWire["output"].string.isEmpty)
        let failed = try HandSession.toolOutput(.object(["error": .string("Photos permission denied")]), success: false, name: "read_photo")
        XCTAssertFalse(failed["success"].bool)
        XCTAssertTrue(failed["output"].string.contains("Photos permission denied"))
    }

    func testRenditionRejectsInvalidBytesExcessiveDimensionsAndSymlinkExport() async throws {
        XCTAssertThrowsError(try HandPhotoRendition.dimensions(Data("not an image".utf8)))
        XCTAssertThrowsError(try HandPhotoRendition.dimensions(Data(repeating: 0, count: HandPhotoRendition.maxBytes + 1)))
        XCTAssertThrowsError(try HandPhotoRendition.dimensions(jpeg(width: 2049, height: 1)))
        let root = try directory(), outside = try directory()
        try FileManager.default.createSymbolicLink(atPath: root.appendingPathComponent("photos").path, withDestinationPath: outside.path)
        let hand = try HandWorkspace(id: "phone-photo", name: "iPhone", root: root)
        do { _ = try await hand.savePhoto(jpeg(), id: "test"); XCTFail("Photo escaped through a symlink") } catch { }
        XCTAssertTrue(try FileManager.default.contentsOfDirectory(atPath: outside.path).isEmpty)
    }

    @MainActor
    func testLargestPhotoReceiptFitsHostedSQLiteRow() throws {
        // The service reparses JSON and persists JSON.stringify(output), which
        // does not escape slashes. Include both image representations.
        let data = Data(repeating: 255, count: HandPhotoRendition.maxBytes)
        let result = try HandPhotoRendition.result(data, id: String(repeating: "a", count: 1024),
            path: "/workspace/photos/00000000-0000-4000-8000-000000000000.jpg", width: 2048, height: 2048)
        let output = try HandSession.toolOutput(result, success: true, name: "read_photo")
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        let receipt: JSON = .object(["type": .string("result"), "outcome": .object([
            "status": .string("completed"), "output": output])])
        XCTAssertLessThan(try encoder.encode(receipt).count, 1_500_000,
            "Reserve at least 500 KB of the 2 MB row limit for call metadata")
    }

    func testCatalogProvidesStrictAssetIDOnlyAndExplainsRendition() {
        let catalog = HandPersonalTools.catalog { name, description, properties, required in
            .object(["name": .string(name), "description": .string(description), "properties": .object(properties), "required": .array(required.map(JSON.string))])
        }
        let read = catalog.first { $0["name"].string == "read_photo" }
        XCTAssertNotNil(read)
        XCTAssertEqual(read?["required"], .array([.string("id")]))
        XCTAssertTrue(read?["description"].string.contains("image(result.content[1])") == true)
        XCTAssertThrowsError(try PersonalToolRequest(["id": .string("photo"), "path": .string("../outside")], allowed: ["id"]))
        XCTAssertThrowsError(try PersonalToolRequest([:], allowed: ["id"]).text("id", required: true))
    }

    @MainActor
    func testPublishedImagesSurviveDraftRemovalAndProduceTypedOutput() async throws {
        let root = try directory(), drafts = try directory()
        let hand = try HandWorkspace(id: "phone-images", name: "iPhone", root: root)
        let data = try jpeg(width: 3000, height: 1000)
        let source = drafts.appendingPathComponent("original.jpg")
        let preview = drafts.appendingPathComponent("preview.jpg")
        try data.write(to: source); try data.write(to: preview)
        var paths: [String] = []
        var attachments: [MessageAttachment] = []
        for _ in 0..<2 {
            let attachment = try MessageAttachment(name: "photo.jpg", byteCount: data.count)
            attachments.append(attachment)
            let path = try await hand.publishImage(attachment: attachment, source: source, preview: preview)
            paths.append(path)
            XCTAssertEqual(path, "/workspace/attachments/" + attachment.id.lowercased() + "/original.jpg")
            XCTAssertEqual(try Data(contentsOf: root.appendingPathComponent(String(path.dropFirst(11)))), data)
        }
        XCTAssertNotEqual(paths[0], paths[1])
        try FileManager.default.removeItem(at: source)
        try FileManager.default.removeItem(at: preview)
        let again = try await hand.publishImage(attachment: attachments[0], source: source, preview: preview)
        XCTAssertEqual(again, paths[0])
        for path in paths + [paths[0].replacingOccurrences(of: "original.jpg", with: "preview.jpg")] {
            let result = try await hand.call(name: "view_image", input: .object(["path": .string(path)]))
            let bytes = try XCTUnwrap(Data(base64Encoded: result["content"].array[1]["data"].string))
            let size = try HandPhotoRendition.dimensions(bytes)
            XCTAssertLessThanOrEqual(max(size.0, size.1), 2048)
            XCTAssertLessThanOrEqual(bytes.count, 512 * 1024)
            let wire = try HandSession.toolOutput(result, success: true, name: "view_image")
            XCTAssertEqual(wire["output"].array[1]["type"].string, "input_image")
            XCTAssertEqual(wire["structured_result"], result)
        }
    }

    func testLocalImageLookupChecksOwnershipAndSurvivesWorkspaceRecreation() async throws {
        let root = try directory(), drafts = try directory(), data = try jpeg()
        let source = drafts.appendingPathComponent("photo.jpg")
        try data.write(to: source)
        let attachment = try MessageAttachment(name: "photo.jpg", byteCount: data.count)
        let hand = try HandWorkspace(id: "phone-images", name: "iPhone", root: root)
        XCTAssertNil(hand.localImageURL(attachment: attachment, preview: false))
        _ = try await hand.publishImage(attachment: attachment, source: source, preview: source)
        try FileManager.default.removeItem(at: source)
        let reopened = try HandWorkspace(id: "phone-images", name: "iPhone", root: root)
        XCTAssertNotNil(reopened.localImageURL(attachment: attachment, preview: true))
        let owned = try MessageAttachment(id: attachment.id, name: attachment.name, byteCount: attachment.byteCount, handID: "phone-images")
        let foreign = try MessageAttachment(id: attachment.id, name: attachment.name, byteCount: attachment.byteCount, handID: "other-phone")
        let original = try XCTUnwrap(reopened.localImageURL(attachment: owned, preview: false))
        XCTAssertEqual(try Data(contentsOf: original), data)
        XCTAssertNil(reopened.localImageURL(attachment: foreign, preview: false))
        let preview = try XCTUnwrap(reopened.localImageURL(attachment: owned, preview: true))
        try FileManager.default.removeItem(at: preview)
        try FileManager.default.createSymbolicLink(at: preview, withDestinationURL: original)
        XCTAssertNil(reopened.localImageURL(attachment: owned, preview: true))
    }

    func testViewImageAndPublicationRejectWorkspaceEscapes() async throws {
        let root = try directory(), outside = try directory()
        let hand = try HandWorkspace(id: "phone-images", name: "iPhone", root: root)
        let source = outside.appendingPathComponent("photo.jpg"), data = try jpeg()
        try data.write(to: source)
        try FileManager.default.createSymbolicLink(at: root.appendingPathComponent("escape"), withDestinationURL: outside)
        for path in ["../photo.jpg", "/workspace/../photo.jpg", source.path, "escape/photo.jpg"] {
            do { _ = try await hand.call(name: "view_image", input: .object(["path": .string(path)])); XCTFail("Allowed escape") } catch { }
        }
        try FileManager.default.createSymbolicLink(at: root.appendingPathComponent("attachments"), withDestinationURL: outside)
        do {
            _ = try await hand.publishImage(attachment: MessageAttachment(name: "photo.jpg", byteCount: data.count), source: source, preview: source)
            XCTFail("Published through symlink")
        } catch { }
        XCTAssertEqual(try FileManager.default.contentsOfDirectory(atPath: outside.path), ["photo.jpg"])
    }

    func testImageIOBoundsLargePreviewAndAppliesOrientation() throws {
        let root = try directory(), source = root.appendingPathComponent("oriented.jpg")
        let input = try XCTUnwrap(CGImageSourceCreateWithData(jpeg(width: 3000, height: 1000) as CFData, nil))
        let bytes = NSMutableData()
        let destination = try XCTUnwrap(CGImageDestinationCreateWithData(bytes, "public.jpeg" as CFString, 1, nil))
        CGImageDestinationAddImageFromSource(destination, input, 0, [kCGImagePropertyOrientation: 6] as CFDictionary)
        XCTAssertTrue(CGImageDestinationFinalize(destination))
        // Padding models a large valid draft JPEG without changing its pixels.
        var padded = bytes as Data
        padded.append(Data(repeating: 0, count: 1024 * 1024))
        try padded.write(to: source)
        let bounded = try HandPhotoRendition.prepare(url: source)
        let dimensions = try HandPhotoRendition.dimensions(bounded)
        XCTAssertGreaterThan(dimensions.1, dimensions.0)
        XCTAssertLessThanOrEqual(bounded.count, 512 * 1024)
    }

    #if os(iOS)
    @MainActor
    func testUIKitRenditionBoundsPixelsAndAppliesOrientation() throws {
        let source = try XCTUnwrap(UIImage(data: jpeg(width: 3000, height: 1000))?.cgImage)
        let oriented = UIImage(cgImage: source, scale: 1, orientation: .right)
        let data = try IOSPhotoReader.encode(oriented)
        let size = try HandPhotoRendition.dimensions(data)
        XCTAssertLessThanOrEqual(max(size.0, size.1), 2048)
        XCTAssertGreaterThan(size.1, size.0)
        XCTAssertLessThanOrEqual(data.count, HandPhotoRendition.maxBytes)
    }
    #endif
}
