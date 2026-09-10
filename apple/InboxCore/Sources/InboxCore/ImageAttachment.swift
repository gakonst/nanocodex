import Foundation

/// Images share the original-file upload path used by videos. The Rust model
/// consumes ordinary text parts and opens the referenced file with its tools.
enum ImageAttachmentContent {
    static let prefix = "Attached original image file.\n[Image attachment]\n"
    static func path(id: String, mediaType: String) -> String {
        let suffix = mediaType == "image/jpeg" ? "jpg" : String(mediaType.dropFirst("image/".count))
        return "/brain/attachments/" + id.lowercased() + "/original." + suffix
    }
    static func original(_ attachment: MessageAttachment) -> [JSON] {
        let header: JSON = .object(["id": .string(attachment.id), "name": .string(attachment.name),
            "path": .string(attachment.originalPath),
            "preview_path": .string("/brain/attachments/" + attachment.id.lowercased() + "/preview.jpg"), "media_type": .string(attachment.mediaType), "size": .number(Double(attachment.byteCount))])
        let encoded = (try? VideoAttachmentContent.encoder.encode(header)).map { String(decoding: $0, as: UTF8.self) } ?? ""
        return [.object(["type": .string("text"), "text": .string(prefix + encoded + "\nUse image tools to inspect the original at path with its full resolution and original bytes preserved. A JPEG at preview_path is available for quick previews or formats unsupported by the image tool.")])]
    }
    static func project(_ content: [JSON]) -> (images: [MessageAttachment], remaining: [JSON]) {
        var images: [MessageAttachment] = [], remaining: [JSON] = []
        for part in content {
            let text = part["text"].string
            if part["type"].string == "text", text.hasPrefix(prefix),
               let line = text.dropFirst(prefix.count).split(separator: "\n").first,
               let header = try? JSONDecoder().decode(JSON.self, from: Data(line.utf8)),
               case .number(let size) = header["size"], let count = Int(exactly: size), count > 0,
               let attachment = try? MessageAttachment(id: header["id"].string, name: header["name"].string,
                    mediaType: header["media_type"].string, byteCount: count),
               header["path"].string == attachment.originalPath {
                images.append(attachment)
            } else { remaining.append(part) }
        }
        return (images, remaining)
    }
}

public extension MessageAttachment {
    func originalContent(path: String) throws -> [JSON] {
        guard path == originalPath else { throw AttachmentError.invalidReference }
        return isVideo ? try originalVideoContent(path: path) : ImageAttachmentContent.original(self)
    }
}
