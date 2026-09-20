import Foundation
import ImageIO
import InboxCore

/// Shared validation keeps Photos output bounded before filesystem and wire IO.
enum HandPhotoRendition {
    static let maxPixels = 2048
    // The hosted receipt stores both the model image and structured MCP result
    // in one SQLite row. Leave room for both base64 copies and call metadata
    // under the Durable Objects 2 MB row limit.
    // https://developers.cloudflare.com/durable-objects/platform/limits/
    static let maxBytes = 512 * 1024

    static func dimensions(_ data: Data) throws -> (Int, Int) {
        guard !data.isEmpty, data.count <= maxBytes,
              let image = CGImageSourceCreateWithData(data as CFData, [kCGImageSourceShouldCache: false] as CFDictionary),
              CGImageSourceGetType(image) as String? == "public.jpeg",
              CGImageSourceGetCount(image) == 1,
              let properties = CGImageSourceCopyPropertiesAtIndex(image, 0, nil) as? [CFString: Any],
              let width = properties[kCGImagePropertyPixelWidth] as? Int,
              let height = properties[kCGImagePropertyPixelHeight] as? Int,
              (1...maxPixels).contains(width), (1...maxPixels).contains(height) else {
            throw HandFailure.contextAccess("The photo could not be rendered as a bounded JPEG.")
        }
        return (width, height)
    }

    static func result(_ data: Data, id: String, path: String, width: Int, height: Int) throws -> JSON {
        let metadata: JSON = .object([
            "id": .string(id), "path": .string(path), "media_type": .string("image/jpeg"),
            "width": .number(Double(width)), "height": .number(Double(height)), "size": .number(Double(data.count)),
            "rendition": .string("oriented inspection JPEG; not the original"),
            "scope": .string("this phone's app workspace; read_file accepts UTF-8 only, use the returned image content")
        ])
        let encoder = JSONEncoder(); encoder.outputFormatting = [.sortedKeys]
        return .object([
            "path": .string(path), "id": .string(id), "content": .array([
                .object(["type": .string("text"), "text": .string(String(decoding: try encoder.encode(metadata), as: UTF8.self))]),
                .object(["type": .string("image"), "mimeType": .string("image/jpeg"), "data": .string(data.base64EncodedString())])
            ])
        ])
    }
}
