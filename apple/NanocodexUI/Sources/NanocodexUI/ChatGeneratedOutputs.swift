import ImageIO
import SwiftUI
import UniformTypeIdentifiers

public struct ChatGeneratedOutputs: View {
    public let outputs: [ChatGeneratedOutput]
    public init(outputs: [ChatGeneratedOutput]) { self.outputs = outputs }

    public var body: some View {
        LazyVStack(alignment: .leading, spacing: 14) {
            ForEach(outputs) { output in
                ChatGeneratedOutputView(output: output)
            }
        }.frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityElement(children: .contain).accessibilityIdentifier("generated-outputs")
    }
}

/// A single transcript row whose parent owns layout and scroll identity.
public struct ChatGeneratedOutputView: View {
    public let output: ChatGeneratedOutput
    public let loadsThumbnail: Bool
    public init(output: ChatGeneratedOutput, loadsThumbnail: Bool = true) {
        self.output = output; self.loadsThumbnail = loadsThumbnail
    }
    public var body: some View {
        Group {
            switch output.kind {
            case .text: ChatMarkdown(text: output.text)
            case .image: GeneratedImage(output: output, loadsThumbnail: loadsThumbnail)
            case .audio, .video: GeneratedMedia(output: output)
            case .file: GeneratedFile(output: output)
            case .unsupported:
                VStack(alignment: .leading, spacing: 4) {
                    Label(output.title, systemImage: "doc").font(.subheadline.weight(.medium))
                    Text(output.text).font(.caption).foregroundStyle(.secondary)
                }.accessibilityIdentifier("generated-unavailable")
            }
        }.frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// User images use the same bounded thumbnail and original-file preview as outputs.
public struct ChatImageAttachment: View {
    let source: String
    @State private var output: ChatGeneratedOutput?
    @State private var loaded = false
    public init(source: String) { self.source = source }
    public var body: some View {
        Group {
            if let output { GeneratedImage(output: output) }
            else if loaded { Label("Image unavailable", systemImage: "photo").foregroundStyle(.secondary) }
            else { ProgressView() }
        }.frame(height: 360, alignment: .topLeading)
        .task(id: source) {
            loaded = false; output = nil
            let source = source
            let parsed = await Task.detached(priority: .utility) {
                ChatGeneratedOutput.image(source: source)
            }.value
            guard !Task.isCancelled else { return }
            output = parsed; loaded = true
        }
    }
}

private struct GeneratedImage: View {
    let output: ChatGeneratedOutput
    var loadsThumbnail = true
    @State private var thumbnail: CGImage?
    @State private var failed = false

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let thumbnail {
                ChatMediaPreview(title: output.title, load: { [try await GeneratedAsset.previewURL(output)] }) {
                    Image(decorative: thumbnail, scale: 1).resizable().aspectRatio(contentMode: .fit)
                        .clipShape(RoundedRectangle(cornerRadius: 12))
                        .frame(maxWidth: 640, maxHeight: 360, alignment: .leading)
                        .accessibilityLabel("Open " + output.title).accessibilityIdentifier("generated-image-loaded")
                }
            } else if failed {
                Label("Image unavailable", systemImage: "photo").foregroundStyle(.secondary)
            } else {
                ProgressView().accessibilityLabel("Loading image").frame(minHeight: 100)
            }
        // A thumbnail owns a stable slot while decoding or downloading. Its
        // original remains available in Quick Look without moving nearby rows.
        }.frame(height: 360, alignment: .topLeading)
            .accessibilityElement(children: .contain).accessibilityIdentifier("generated-image")
            .task(id: loadsThumbnail ? output.id : nil) {
                thumbnail = nil; failed = false
                guard loadsThumbnail else { return }
                do {
                    let decoded = try await GeneratedAsset.thumbnail(output)
                    guard !Task.isCancelled else { return }
                    thumbnail = decoded; failed = decoded == nil
                } catch { if !Task.isCancelled { failed = true } }
            }
    }
}

private struct GeneratedMedia: View {
    let output: ChatGeneratedOutput
    var body: some View {
        ChatMediaPreview(title: output.title, load: { [try await GeneratedAsset.previewURL(output)] }) {
            Label(output.title, systemImage: output.kind == .video ? "play.rectangle" : "play.circle")
                .font(.subheadline).frame(minHeight: 44)
                .accessibilityLabel(output.kind == .video ? "Play video" : "Play audio")
                .accessibilityIdentifier("generated-" + output.kind.rawValue + "-play")
        }.accessibilityElement(children: .contain).accessibilityIdentifier("generated-" + output.kind.rawValue)
    }
}

private struct GeneratedFile: View {
    let output: ChatGeneratedOutput
    @State private var file: URL?
    @State private var failed = false

    var body: some View {
        Group {
            if let source = output.source, !source.hasPrefix("data:"), let url = URL(string: source) {
                Link(destination: url) { Label(output.title, systemImage: "arrow.down.doc") }
            } else if let file {
                ShareLink(item: file) { Label(output.title, systemImage: "arrow.down.doc") }
            } else if failed {
                Label(output.title + " · unavailable", systemImage: "doc")
            } else { ProgressView(output.title) }
        }.font(.subheadline).accessibilityIdentifier("generated-file")
            .task(id: output.id) {
                file = nil; failed = false
                guard output.source?.hasPrefix("data:") == true else { return }
                do {
                    let url = try await GeneratedAsset.playableURL(output)
                    guard !Task.isCancelled else { return }
                    file = url
                }
                catch { if !Task.isCancelled { failed = true } }
            }
    }
}

enum GeneratedAsset {
    enum Failure: Error { case unavailable }
    static let thumbnails: NSCache<NSString, CGImage> = {
        let cache = NSCache<NSString, CGImage>()
        cache.totalCostLimit = 32 * 1024 * 1024; cache.countLimit = 24
        return cache
    }()
    static let session: URLSession = {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.httpShouldSetCookies = false; configuration.httpCookieStorage = nil
        configuration.timeoutIntervalForRequest = 30
        return URLSession(configuration: configuration)
    }()

    static func thumbnail(_ output: ChatGeneratedOutput) async throws -> CGImage? {
        if let cached = thumbnails.object(forKey: output.id as NSString) { return cached }
        let url: URL
        let downloaded: Bool
        if output.source?.hasPrefix("data:") == true {
            url = try await playableURL(output); downloaded = false
        } else {
            guard let source = output.source, let remote = URL(string: source),
                  ["https", "http"].contains(remote.scheme) else { throw Failure.unavailable }
            let (file, response) = try await session.download(from: remote)
            guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
                try? FileManager.default.removeItem(at: file); throw Failure.unavailable
            }
            url = file; downloaded = true
        }
        defer { if downloaded { try? FileManager.default.removeItem(at: url) } }
        try Task.checkCancellation()
        let decoding = Task.detached(priority: .utility) { () throws -> CGImage? in
            try Task.checkCancellation()
            guard let source = CGImageSourceCreateWithURL(url as CFURL, nil) else { return nil }
            return CGImageSourceCreateThumbnailAtIndex(source, 0, [
                kCGImageSourceCreateThumbnailFromImageAlways: true,
                kCGImageSourceCreateThumbnailWithTransform: true,
                kCGImageSourceThumbnailMaxPixelSize: 1600,
                kCGImageSourceShouldCacheImmediately: true,
            ] as CFDictionary)
        }
        let image = try await withTaskCancellationHandler(operation: { try await decoding.value }, onCancel: { decoding.cancel() })
        try Task.checkCancellation()
        if let image { thumbnails.setObject(image, forKey: output.id as NSString, cost: image.bytesPerRow * image.height) }
        return image
    }

    static func previewURL(_ output: ChatGeneratedOutput) async throws -> URL {
        let source = try await playableURL(output)
        let suffix = UTType(mimeType: output.mimeType ?? "")?.preferredFilenameExtension
            ?? (source.pathExtension.isEmpty ? "bin" : source.pathExtension)
        let target = FileManager.default.temporaryDirectory
            .appendingPathComponent("attachment-" + UUID().uuidString).appendingPathExtension(suffix)
        if source.isFileURL { try FileManager.default.copyItem(at: source, to: target) }
        else {
            let (download, response) = try await session.download(from: source)
            defer { try? FileManager.default.removeItem(at: download) }
            guard let response = response as? HTTPURLResponse, (200..<300).contains(response.statusCode) else { throw Failure.unavailable }
            try FileManager.default.moveItem(at: download, to: target)
        }
        if Task.isCancelled { try? FileManager.default.removeItem(at: target); throw CancellationError() }
        return target
    }

    static func playableURL(_ output: ChatGeneratedOutput) async throws -> URL {
        guard let source = output.source else { throw Failure.unavailable }
        if !source.hasPrefix("data:"), let url = URL(string: source), ["https", "http"].contains(url.scheme) { return url }
        let writing = Task.detached(priority: .utility) { try embeddedURL(output, source: source) }
        return try await withTaskCancellationHandler(operation: { try await writing.value }, onCancel: { writing.cancel() })
    }

    private static func embeddedURL(_ output: ChatGeneratedOutput, source: String) throws -> URL {
        try Task.checkCancellation()
        guard source.hasPrefix("data:"), let comma = source.firstIndex(of: ","),
              source[..<comma].hasSuffix(";base64") else { throw Failure.unavailable }
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("CentaurGeneratedOutputs", isDirectory: true)
        let folder = directory.appendingPathComponent(output.id, isDirectory: true)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        let extensions = ["audio/wav": "wav", "audio/mpeg": "mp3", "audio/mp4": "m4a", "video/mp4": "mp4", "text/html": "html", "text/csv": "csv", "application/pdf": "pdf", "application/json": "json", "image/svg+xml": "svg", "text/plain": "txt", "text/markdown": "md"]
        let suffix = extensions[output.mimeType ?? ""] ?? UTType(mimeType: output.mimeType ?? "")?.preferredFilenameExtension ?? "bin"
        // The content hash is filesystem-safe regardless of title length.
        // The original title remains the visible/share label.
        let url = folder.appendingPathComponent(output.id).appendingPathExtension(suffix)
        if FileManager.default.fileExists(atPath: url.path) { return url }
        let temporary = folder.appendingPathComponent(UUID().uuidString + ".tmp")
        guard FileManager.default.createFile(atPath: temporary.path, contents: nil) else { throw Failure.unavailable }
        defer { try? FileManager.default.removeItem(at: temporary) }
        let file = try FileHandle(forWritingTo: temporary)
        do {
            // Chunk size only controls working memory. No source/output size
            // limit: each complete base64 quartet is decoded once into the file.
            let bytes = source[source.index(after: comma)...].utf8
            var offset = bytes.startIndex
            while offset != bytes.endIndex {
                try Task.checkCancellation()
                let end = bytes.index(offset, offsetBy: 64 * 1024, limitedBy: bytes.endIndex) ?? bytes.endIndex
                let encoded = Data(bytes[offset..<end])
                if let padding = encoded.firstIndex(of: 61) {
                    guard end == bytes.endIndex, encoded[padding...].allSatisfy({ $0 == 61 }) else { throw Failure.unavailable }
                }
                guard let decoded = Data(base64Encoded: encoded) else { throw Failure.unavailable }
                try file.write(contentsOf: decoded); offset = end
            }
            try Task.checkCancellation()
            try file.close()
        } catch { try? file.close(); throw error }
        do { try FileManager.default.moveItem(at: temporary, to: url) }
        catch { if !FileManager.default.fileExists(atPath: url.path) { throw error } }
        return url
    }
}
