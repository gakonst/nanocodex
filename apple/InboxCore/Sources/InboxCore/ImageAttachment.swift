import Foundation

/// Shared user-input projection for the phone and desktop. Attachment transport
/// descriptors belong to media controls inside the user's message, never prose.
public struct TranscriptInput: Equatable, Sendable {
    public let text: String
    public let images: [String]
    public let imageFiles: [MessageAttachment]
    public let videos: [TranscriptVideo]

    public init(_ input: JSON) {
        let parts = input.array.isEmpty ? [.object(["type": JSON.string("text"), "text": input])] : input.array
        let normalized = parts.flatMap { part -> [JSON] in
            let type = part["type"].string
            if ["image", "input_image", "image_url"].contains(type) {
                let url = part["image_url"].string.isEmpty ? part["image_url"]["url"].string : part["image_url"].string
                return [.object(["type": .string("image"), "image_url": .string(url)])]
            }
            if ["text", "input_text"].contains(type) { return Self.textParts(part["text"].string) }
            return [part]
        }
        let files = ImageAttachmentContent.project(normalized)
        let media = VideoAttachmentContent.project(files.remaining)
        imageFiles = files.images; videos = media.videos
        images = media.remaining.filter { $0["type"].string == "image" }.map { $0["image_url"].string }
        text = media.remaining.compactMap {
            $0["type"].string == "image" ? nil : $0["type"].string == "audio" ? "[Audio]" : $0["text"].string
        }.filter { !$0.isEmpty }.joined(separator: "\n")
    }

    private static func textParts(_ text: String) -> [JSON] {
        let formats = [
            (ImageAttachmentContent.prefix, "A JPEG at preview_path is available for quick previews or formats unsupported by the image tool."),
            (VideoAttachmentContent.originalPrefix, "The original file is available at this filesystem path. Use tools to inspect it; its audio tracks are preserved.")
        ]
        var rest = text, result: [JSON] = []
        func part(_ value: String) -> JSON { .object(["type": .string("text"), "text": .string(value)]) }
        while let found = formats.compactMap({ format -> (Range<String.Index>, String)? in
            rest.range(of: format.0).map { ($0, format.1) }
        }).min(by: { $0.0.lowerBound < $1.0.lowerBound }),
              let end = rest.range(of: found.1, range: found.0.upperBound..<rest.endIndex) {
            let before = String(rest[..<found.0.lowerBound]).trimmingCharacters(in: .whitespacesAndNewlines)
            if !before.isEmpty { result.append(part(before)) }
            result.append(part(String(rest[found.0.lowerBound..<end.upperBound])))
            rest = String(rest[end.upperBound...]).trimmingCharacters(in: .whitespacesAndNewlines)
        }
        if !rest.isEmpty { result.append(part(rest)) }
        return result
    }
}

/// Images share the original-file upload path used by videos. The Rust model
/// consumes ordinary text parts and opens the referenced file with its tools.
enum ImageAttachmentContent {
    static let prefix = "Attached original image file.\n[Image attachment]\n"
    static func path(id: String, mediaType: String) -> String {
        let suffix = mediaType == "image/jpeg" ? "jpg" : String(mediaType.dropFirst("image/".count))
        return "/brain/attachments/" + id.lowercased() + "/original." + suffix
    }
    static func original(_ attachment: MessageAttachment) -> [JSON] {
        var fields: [String: JSON] = ["id": .string(attachment.id), "name": .string(attachment.name),
            "path": .string(attachment.originalPath),
            "preview_path": .string((attachment.handID == nil ? "/brain/attachments/" : "/workspace/attachments/") + attachment.id.lowercased() + "/preview.jpg"), "media_type": .string(attachment.mediaType), "size": .number(Double(attachment.byteCount))]
        if let handID = attachment.handID { fields["hand_id"] = .string(handID) }
        let header = JSON.object(fields)
        let encoded = (try? VideoAttachmentContent.encoder.encode(header)).map { String(decoding: $0, as: UTF8.self) } ?? ""
        let location = attachment.handID.map { "This image is local to the phone Hand with id \($0). Discover that Hand’s view_image tool and pass path (or preview_path); these paths are relative to that phone, whose connection must be available. Use image(result.content[1]) to display its returned image. " } ?? ""
        return [.object(["type": .string("text"), "text": .string(prefix + encoded + "\n" + location + "Use view_image on path to inspect this image; large originals are resized for inspection without loading them into the brain. preview_path is a model-viewable JPEG fallback, oriented and bounded to 2048 pixels and 2 MiB. The original at path preserves the full resolution and original bytes for tasks that need them. A JPEG at preview_path is available for quick previews or formats unsupported by the image tool.")])]
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
                    mediaType: header["media_type"].string, byteCount: count,
                    handID: header["hand_id"] == .null ? nil : header["hand_id"].string),
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
