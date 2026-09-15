import CryptoKit
import Foundation

/// Generated content is separate from tool diagnostics and has a stable identity
/// across nested calls, streamed updates, and replayed results.
public struct ChatGeneratedOutput: Identifiable, Equatable, Hashable, Sendable {
    public enum Kind: String, Sendable { case text, image, audio, video, file, unsupported }
    public let id: String
    public let kind: Kind
    public let text: String
    public let source: String?
    public let mimeType: String?
    public let title: String

    private init(kind: Kind, text: String = "", source: String? = nil, mimeType: String? = nil, title: String = "") {
        self.kind = kind; self.text = text; self.source = source; self.mimeType = mimeType; self.title = title
        // Labels/metadata can differ between a nested tool and its outer exec.
        var hash = SHA256()
        hash.update(data: Data((kind.rawValue + "\n").utf8))
        let identity = source ?? (kind == .unsupported ? title + "\n" + text : text)
        let bytes = identity.utf8
        var offset = bytes.startIndex
        while offset != bytes.endIndex {
            let end = bytes.index(offset, offsetBy: 64 * 1024, limitedBy: bytes.endIndex) ?? bytes.endIndex
            hash.update(data: Data(bytes[offset..<end])); offset = end
        }
        id = hash.finalize().map { String(format: "%02x", $0) }.joined()
    }

    public static func parse(results: [String], includeText: Bool = false) -> [Self] {
        var parser = Parser(includesText: includeText)
        for source in results {
            parser.walk(decode(source) ?? source, includeText: includeText)
        }
        return parser.outputs
    }

    /// Attachments already contain a source URL. Apply the same media policy
    /// without serializing and decoding the entire image as an artificial event.
    static func image(source: String) -> Self? {
        var parser = Parser(includesText: false)
        parser.emitSource(source, kind: .image, mime: "image/png", title: "")
        return parser.outputs.first
    }

    /// Keep Activity readable without ever printing an embedded binary payload.
    public static func sanitizedText(_ source: String) -> String {
        let value = sanitized(decode(source) ?? source)
        if let text = value as? String { return text }
        guard let data = try? JSONSerialization.data(withJSONObject: value, options: [.fragmentsAllowed, .sortedKeys, .prettyPrinted]),
              let text = String(data: data, encoding: .utf8) else { return "Generated output" }
        return text
    }

    private static func decode(_ text: String) -> Any? {
        guard let first = text.trimmingCharacters(in: .whitespacesAndNewlines).first, "[{\"".contains(first) else { return nil }
        return try? JSONSerialization.jsonObject(with: Data(text.utf8), options: [.fragmentsAllowed])
    }

    private enum SanitizeStep {
        case value(Any), array(Int), object([String])
    }

    private static func sanitized(_ value: Any) -> Any {
        var work: [SanitizeStep] = [.value(value)]
        var values: [Any] = []
        while let step = work.popLast() {
            switch step {
            case .value(let value):
                if let text = value as? String {
                    if text.prefix(5).lowercased() == "data:" { values.append("Embedded attachment") }
                    else if let decoded = decode(text) { work.append(.value(decoded)) }
                    else {
                        values.append(text.replacingOccurrences(of: #"data:[^\s\)\]\"<>]+"#,
                            with: "[embedded attachment]", options: [.regularExpression, .caseInsensitive]))
                    }
                } else if let array = value as? [Any] {
                    work.append(.array(array.count))
                    work.append(contentsOf: array.reversed().map(SanitizeStep.value))
                } else if let object = value as? [String: Any] {
                    let type = object["type"] as? String ?? ""
                    let binary = ["image", "input_image", "audio", "input_audio", "video", "input_video"].contains(type)
                        || object["mimeType"] != nil || object["mime_type"] != nil
                    let keys = object.keys.sorted()
                    work.append(.object(keys))
                    for key in keys.reversed() {
                        work.append(.value(key == "blob" || (key == "data" && binary)
                            ? "Embedded attachment" : object[key]!))
                    }
                } else { values.append(value) }
            case .array(let count):
                let array = Array(values.suffix(count))
                values.removeLast(count); values.append(array)
            case .object(let keys):
                let object = Dictionary(uniqueKeysWithValues: zip(keys, values.suffix(keys.count)))
                values.removeLast(keys.count); values.append(object)
            }
        }
        return values.first ?? NSNull()
    }

    private struct Parser {
        // Tool content blocks and embedded resources are transport formats,
        // not permission to promote their text into the conversation.
        let includesText: Bool
        var outputs: [ChatGeneratedOutput] = []
        var seen = Set<String>()
        var recognized = 0

        mutating func append(_ output: ChatGeneratedOutput) {
            guard seen.insert(output.id).inserted else { return }
            outputs.append(output)
        }

        private enum Step {
            case value(Any, includeText: Bool)
            case fallback(String, recognized: Int, includeText: Bool)
        }

        mutating func walk(_ value: Any, includeText: Bool) {
            var work: [Step] = [.value(value, includeText: includeText)]
            while let step = work.popLast() {
                switch step {
                case .fallback(let string, let before, let includeText):
                    if recognized == before, includesText, includeText { emitText(string) }
                case .value(let value, let includeText):
                    if let string = value as? String {
                        if let decoded = ChatGeneratedOutput.decode(string) {
                            work.append(.fallback(string, recognized: recognized, includeText: includeText))
                            work.append(.value(decoded, includeText: false))
                        } else if includeText { emitText(string) }
                    } else if let array = value as? [Any] {
                        work.append(contentsOf: array.reversed().map { .value($0, includeText: includeText) })
                    } else { walkObject(value, includeText: includeText, work: &work) }
                }
            }
        }

        private mutating func walkObject(_ value: Any, includeText: Bool, work: inout [Step]) {
            guard let object = value as? [String: Any] else { return }
            let type = object["type"] as? String ?? ""
            // A read tool may return serialized history or an echoed request.
            // Those user attachments keep their original conversation ownership.
            if type == "turn_accepted" || object["role"] as? String == "user" { return }
            let mime = (object["mimeType"] ?? object["mime_type"]) as? String
            let title = (object["title"] as? String) ?? (object["name"] as? String) ?? ""
            if ["input_text", "text", "output_text", "image", "input_image", "image_url", "audio", "input_audio", "output_audio", "video", "input_video", "resource_link", "file", "input_file", "output_file", "resource", "unsupported"].contains(type) { recognized += 1 }
            switch type {
            case "input_text", "text", "output_text":
                if let text = object["text"] { work.append(.value(text, includeText: true)) }
                return
            case "image", "input_image", "image_url":
                emitAsset(object, kind: .image, mime: mime ?? "image/png", title: title); return
            case "audio", "input_audio", "output_audio":
                emitAsset(object, kind: .audio, mime: mime ?? audioMime(object["format"] as? String), title: title); return
            case "video", "input_video":
                emitAsset(object, kind: .video, mime: mime ?? "video/mp4", title: title); return
            case "resource_link", "file", "input_file", "output_file":
                emitAsset(object, kind: kind(for: mime), mime: mime, title: title); return
            case "resource":
                if let resource = object["resource"] { walkResource(resource) }
                return
            case "unsupported":
                append(.init(kind: .unsupported, text: "This generated output is unavailable here.", title: title.isEmpty ? "Generated output" : title)); return
            default: break
            }
            if object["image_url"] != nil {
                recognized += 1
                emitAsset(object, kind: .image, mime: mime ?? "image/png", title: title)
                if let hint = object["output_hint"] as? String { emitText(hint) }
            } else if object["audio_url"] != nil {
                recognized += 1
                emitAsset(object, kind: .audio, mime: mime ?? "audio/mpeg", title: title)
            } else if object["video_url"] != nil {
                recognized += 1
                emitAsset(object, kind: .video, mime: mime ?? "video/mp4", title: title)
            } else if mime != nil, object["url"] != nil || object["uri"] != nil || object["blob"] != nil {
                recognized += 1
                walkResource(object)
            }
            for key in ["content", "result", "structured_result", "structuredContent", "output", "outputs", "attachments", "artifacts", "files", "images", "data"].reversed() {
                if let child = object[key], child is [Any] || child is [String: Any] || key != "data" {
                    work.append(.value(child, includeText: includeText && ["result", "output"].contains(key)))
                }
            }
        }

        mutating func walkResource(_ value: Any) {
            guard let resource = value as? [String: Any] else { return }
            let mime = (resource["mimeType"] ?? resource["mime_type"]) as? String
            let title = (resource["title"] ?? resource["name"]) as? String ?? resourceTitle(resource["uri"] as? String)
            if let text = resource["text"] as? String {
                if mime == "text/html" || mime == "image/svg+xml" {
                    emitSource("data:\(mime!);base64," + Data(text.utf8).base64EncodedString(), kind: .file, mime: mime, title: title)
                } else {
                    emitText(text)
                    let type = mime ?? "text/plain"
                    emitSource("data:\(type);base64," + Data(text.utf8).base64EncodedString(), kind: .file, mime: type, title: title)
                }
            } else {
                emitAsset(resource, kind: kind(for: mime), mime: mime, title: title)
            }
        }

        mutating func emitAsset(_ value: [String: Any], kind: Kind, mime: String?, title: String) {
            var object = value, kind = kind, mime = mime
            while true {
                if let blob = (object["data"] ?? object["blob"]) as? String, let mime {
                    emitSource(blob.hasPrefix("data:") ? blob : "data:\(mime);base64," + blob, kind: kind, mime: mime, title: title); return
                }
                for key in ["image_url", "audio_url", "video_url", "url", "uri", "file_url"] {
                    if let source = object[key] as? String { emitSource(source, kind: kind, mime: mime, title: title); return }
                    if let nested = object[key] as? [String: Any], let source = nested["url"] as? String {
                        emitSource(source, kind: kind, mime: mime, title: title); return
                    }
                }
                guard let audio = object["input_audio"] as? [String: Any] else {
                    append(.init(kind: .unsupported, text: "This generated attachment is unavailable here.", title: title.isEmpty ? "Generated file" : title)); return
                }
                object = audio; kind = .audio; mime = audioMime(audio["format"] as? String)
            }
        }

        mutating func emitSource(_ source: String, kind: Kind, mime: String?, title: String) {
            let clean = source.trimmingCharacters(in: .whitespacesAndNewlines)
            let label = title.isEmpty ? (kind == .file ? resourceTitle(clean) : "Generated " + kind.rawValue) : title
            if clean.prefix(5).lowercased() == "data:", let comma = clean.firstIndex(of: ","),
               clean[..<comma].lowercased().hasSuffix(";base64") {
                let actualMime = String(clean[clean.index(clean.startIndex, offsetBy: 5)..<comma]).lowercased().components(separatedBy: ";").first ?? mime
                let safeKind: Kind = actualMime == "text/html" || actualMime == "image/svg+xml" ? .file : kind
                let normalized = clean[..<comma].lowercased() + clean[comma...]
                append(.init(kind: safeKind, source: normalized, mimeType: actualMime, title: label)); return
            }
            if let url = URL(string: clean), ["https", "http"].contains(url.scheme?.lowercased() ?? ""),
               url.host != nil, url.user == nil, url.password == nil {
                let safeKind: Kind = mime == "text/html" || mime == "image/svg+xml" ? .file : kind
                append(.init(kind: safeKind, source: url.absoluteString, mimeType: mime, title: label)); return
            }
            append(.init(kind: .unsupported, text: "This resource is not available on this device.", title: label))
        }

        mutating func emitText(_ value: String) {
            guard !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
            if value.range(of: #"^Script (completed|running[^\n]*)\nWall time [0-9.]+ seconds\nOutput:\s*$"#, options: .regularExpression) != nil { return }
            if value.hasPrefix("data:"), let type = value.dropFirst(5).split(separator: ";").first {
                emitSource(value, kind: kind(for: String(type)), mime: String(type), title: ""); return
            }
            var cursor = value.startIndex
            // Foundation renders link text but not Markdown image attachments.
            if let pattern = try? NSRegularExpression(pattern: #"(!?)\[([^\]]*)\]\((?:<([^>]+)>|([^\s\)]+))\)"#) {
                let original = value as NSString
                for match in pattern.matches(in: value, range: NSRange(location: 0, length: original.length)) {
                    let source = original.substring(with: match.range(at: match.range(at: 3).location == NSNotFound ? 4 : 3))
                    let title = original.substring(with: match.range(at: 2))
                    let isImage = match.range(at: 1).length > 0
                    let isFile = ["pdf", "csv", "zip", "html", "json", "txt", "md", "xlsx", "docx", "pptx"].contains(URL(string: source)?.pathExtension.lowercased() ?? "")
                    let unsupported = !["https", "http"].contains(URL(string: source)?.scheme?.lowercased() ?? "")
                    if (isImage || isFile || unsupported), let range = Range(match.range, in: value) {
                        emitMarkdown(String(value[cursor..<range.lowerBound]))
                        emitSource(source, kind: isImage ? .image : .file, mime: nil, title: title)
                        cursor = range.upperBound
                    }
                }
            }
            emitMarkdown(String(value[cursor...]))
        }

        mutating func emitMarkdown(_ value: String) {
            guard includesText else { return }
            let text = ChatGeneratedOutput.sanitizedText(value)
            if !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { append(.init(kind: .text, text: text)) }
        }

        func kind(for mime: String?) -> Kind {
            if mime?.hasPrefix("image/") == true { return .image }
            if mime?.hasPrefix("audio/") == true { return .audio }
            if mime?.hasPrefix("video/") == true { return .video }
            return .file
        }
        func audioMime(_ format: String?) -> String { format == "wav" ? "audio/wav" : "audio/mpeg" }
        func resourceTitle(_ source: String?) -> String {
            guard let source, !source.hasPrefix("data:"), let name = URL(string: source)?.lastPathComponent, !name.isEmpty else { return "Generated file" }
            return name
        }
    }
}
