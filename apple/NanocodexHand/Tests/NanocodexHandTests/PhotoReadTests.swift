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
