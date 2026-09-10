import Foundation
import UniformTypeIdentifiers
import PDFKit
import Vision

/// Both entrypoints use the same on-device text extraction. Files are
/// never uploaded or fetched from their links as a side effect of capture.
public enum ContextImport {
    public static func text(data: Data, type: UTType) throws -> String {
        let text: String
        if type.conforms(to: .image) {
            let request = VNRecognizeTextRequest()
            request.recognitionLevel = .accurate
            try VNImageRequestHandler(data: data).perform([request])
            text = (request.results ?? []).compactMap { $0.topCandidates(1).first?.string }.joined(separator: "\n")
        } else if type.conforms(to: .pdf) {
            guard let document = PDFDocument(data: data), !document.isLocked else { throw CaptureError.unreadable }
            text = document.string ?? ""
        } else if type.conforms(to: .plainText) {
            guard let decoded = String(data: data, encoding: .utf8) ?? String(data: data, encoding: .utf16) else { throw CaptureError.unreadable }
            text = decoded
        } else { throw CaptureError.unsupported }
        guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { throw CaptureError.unreadable }
        return text
    }

    public static func suggestedSource(for url: String) -> String {
        guard let host = URL(string: url)?.host?.lowercased() else { return "Shared" }
        let names = ["instagram.com": "Instagram", "whatsapp.com": "WhatsApp", "wa.me": "WhatsApp", "x.com": "X", "twitter.com": "X", "youtube.com": "YouTube", "youtu.be": "YouTube"]
        for (domain, name) in names where host == domain || host.hasSuffix("." + domain) { return name }
        return host.hasPrefix("www.") ? String(host.dropFirst(4)) : host
    }

    public static func load(_ providers: [NSItemProvider]) async throws -> [CaptureInput] {
        guard !providers.isEmpty else { throw CaptureError.empty }
        var items: [CaptureInput] = []
        for provider in providers { items.append(try await load(provider)) }
        // Safari can supply both a URL and the preprocessing result for that URL.
        // Keep the readable page once, independent of provider order.
        return items.enumerated().compactMap { index, item in
            if item.text.isEmpty, !item.url.isEmpty,
               items.contains(where: { $0.url == item.url && !$0.text.isEmpty }) { return nil }
            if item.url.isEmpty, items.contains(where: {
                !$0.url.isEmpty && ($0.url == item.text || $0.text == item.text || $0.text.hasPrefix(item.text + "\n"))
            }) { return nil }
            if items[..<index].contains(item) { return nil }
            return item
        }
    }

    public static func load(_ provider: NSItemProvider) async throws -> CaptureInput {
        if provider.hasItemConformingToTypeIdentifier(UTType.propertyList.identifier) {
            let page: CaptureInput? = try await withCheckedThrowingContinuation { continuation in
                provider.loadItem(forTypeIdentifier: UTType.propertyList.identifier, options: nil) { value, error in
                    if let error { continuation.resume(throwing: error); return }
                    guard let dictionary = value as? [String: Any],
                          let page = dictionary[NSExtensionJavaScriptPreprocessingResultsKey] as? [String: Any],
                          let url = page["url"] as? String, let text = page["text"] as? String else {
                        continuation.resume(returning: nil); return
                    }
                    do {
                        continuation.resume(returning: try CaptureInput(source: suggestedSource(for: url), text: text, url: url).validated())
                    } catch { continuation.resume(throwing: error) }
                }
            }
            if let page { return page }
        }
        if provider.hasItemConformingToTypeIdentifier(UTType.url.identifier), !provider.hasItemConformingToTypeIdentifier(UTType.fileURL.identifier) {
            let link: String = try await withCheckedThrowingContinuation { continuation in
                provider.loadItem(forTypeIdentifier: UTType.url.identifier, options: nil) { value, error in
                    if let error { continuation.resume(throwing: error) }
                    else if let value = value as? URL { continuation.resume(returning: value.absoluteString) }
                    else if let value = value as? String { continuation.resume(returning: value) }
                    else if let value = value as? Data, let text = String(data: value, encoding: .utf8) { continuation.resume(returning: text) }
                    else { continuation.resume(throwing: CaptureError.unsupported) }
                }
            }
            return CaptureInput(source: suggestedSource(for: link), text: "", url: link)
        }
        // Prefer an original image/document representation over a filename that
        // happens to be available as text on the same provider.
        let types = provider.registeredTypeIdentifiers.compactMap { UTType($0) }
        guard let type = types.first(where: { $0.conforms(to: .image) || $0.conforms(to: .pdf) })
            ?? types.first(where: { $0.conforms(to: .plainText) }) else { throw CaptureError.unsupported }
        let payload: (Data, String) = try await withCheckedThrowingContinuation { continuation in
            provider.loadFileRepresentation(forTypeIdentifier: type.identifier) { url, error in
                do {
                    if let error { throw error }
                    guard let url else { throw CaptureError.unreadable }
                    // Read while the provider's temporary file is still valid.
                    continuation.resume(returning: (try Data(contentsOf: url, options: .mappedIfSafe), url.lastPathComponent))
                } catch { continuation.resume(throwing: error) }
            }
        }
        let content = try await Task.detached { try text(data: payload.0, type: type) }.value
        let name = provider.suggestedName ?? (type.conforms(to: .plainText) ? "" : payload.1)
        return CaptureInput(source: "Shared", text: content, filename: name)
    }
}
