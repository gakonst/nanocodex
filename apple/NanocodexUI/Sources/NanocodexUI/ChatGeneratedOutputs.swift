import AVKit
import ImageIO
import SwiftUI

public struct ChatGeneratedOutputs: View {
    public let outputs: [ChatGeneratedOutput]
    public init(outputs: [ChatGeneratedOutput]) { self.outputs = outputs }

    public var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            ForEach(outputs) { output in
                switch output.kind {
                case .text: ChatMarkdown(text: output.text)
                case .image: GeneratedImage(output: output)
                case .audio, .video: GeneratedMedia(output: output)
                case .file: GeneratedFile(output: output)
                case .unsupported:
                    VStack(alignment: .leading, spacing: 4) {
                        Label(output.title, systemImage: "doc").font(.subheadline.weight(.medium))
                        Text(output.text).font(.caption).foregroundStyle(.secondary)
                    }.accessibilityIdentifier("generated-unavailable")
                }
            }
        }.frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityElement(children: .contain).accessibilityIdentifier("generated-outputs")
    }
}

private struct GeneratedImage: View {
    let output: ChatGeneratedOutput
    @State private var thumbnail: CGImage?
    @State private var failed = false

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let thumbnail {
                Image(decorative: thumbnail, scale: 1).resizable().aspectRatio(contentMode: .fit)
                    .frame(maxWidth: 640, maxHeight: 360, alignment: .leading)
                    .accessibilityLabel(output.title).accessibilityIdentifier("generated-image-loaded")
            } else if failed {
                Label("Image unavailable", systemImage: "photo").foregroundStyle(.secondary)
            } else {
                ProgressView("Loading image…").frame(minHeight: 100)
            }
        }.clipShape(RoundedRectangle(cornerRadius: 12))
            .accessibilityElement(children: .contain).accessibilityIdentifier("generated-image")
            .task(id: output.id) {
                thumbnail = nil; failed = false
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
    @Environment(\.scenePhase) private var scenePhase
    @State private var player: AVPlayer?
    @State private var playing = false
    @State private var failed = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(output.title).font(.subheadline.weight(.medium))
            if let player {
                if output.kind == .video {
                    VideoPlayer(player: player).frame(height: 240).clipShape(RoundedRectangle(cornerRadius: 12))
                } else {
                    Button {
                        if playing { player.pause() } else { player.seek(to: .zero); player.play() }
                        playing.toggle()
                    } label: { Label(playing ? "Pause audio" : "Play audio", systemImage: playing ? "pause.fill" : "play.fill") }
                        .buttonStyle(.bordered).accessibilityIdentifier("generated-audio-play")
                }
            } else if failed { Text("Media unavailable").font(.caption).foregroundStyle(.secondary) }
            else { ProgressView("Loading media…") }
        }.accessibilityElement(children: .contain).accessibilityIdentifier("generated-" + output.kind.rawValue)
            .task(id: output.id) {
                player?.pause(); player = nil; playing = false; failed = false
                do {
                    let url = try await GeneratedAsset.playableURL(output)
                    let asset = AVURLAsset(url: url)
                    guard try await asset.load(.isPlayable) else { throw GeneratedAsset.Failure.unavailable }
                    guard !Task.isCancelled else { return }
                    player = AVPlayer(playerItem: AVPlayerItem(asset: asset))
                } catch { if !Task.isCancelled { failed = true } }
            }
            .onDisappear { player?.pause(); playing = false }
            .onChange(of: scenePhase) { _, phase in
                if phase != .active { player?.pause(); playing = false }
            }
            .onReceive(NotificationCenter.default.publisher(for: .AVPlayerItemDidPlayToEndTime)) { event in
                if let item = event.object as? AVPlayerItem, item === player?.currentItem { playing = false }
            }
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

private enum GeneratedAsset {
    enum Failure: Error { case unavailable }
    static let limit = 16 * 1024 * 1024
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
        let data = try await data(output)
        let image = await Task.detached(priority: .utility) { () -> CGImage? in
            guard let source = CGImageSourceCreateWithData(data as CFData, nil) else { return nil }
            return CGImageSourceCreateThumbnailAtIndex(source, 0, [
                kCGImageSourceCreateThumbnailFromImageAlways: true,
                kCGImageSourceCreateThumbnailWithTransform: true,
                kCGImageSourceThumbnailMaxPixelSize: 1600,
                kCGImageSourceShouldCacheImmediately: true,
            ] as CFDictionary)
        }.value
        if let image { thumbnails.setObject(image, forKey: output.id as NSString, cost: image.bytesPerRow * image.height) }
        return image
    }

    static func data(_ output: ChatGeneratedOutput) async throws -> Data {
        guard let source = output.source else { throw Failure.unavailable }
        if source.hasPrefix("data:") {
            return try await Task.detached(priority: .utility) {
                guard let comma = source.firstIndex(of: ","), source[..<comma].hasSuffix(";base64"),
                      let data = Data(base64Encoded: String(source[source.index(after: comma)...])), data.count <= limit else { throw Failure.unavailable }
                return data
            }.value
        }
        guard let url = URL(string: source), ["https", "http"].contains(url.scheme) else { throw Failure.unavailable }
        let (bytes, response) = try await session.bytes(from: url)
        guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode), response.expectedContentLength <= limit else { throw Failure.unavailable }
        var data = Data()
        for try await byte in bytes {
            if data.count >= limit { throw Failure.unavailable }
            data.append(byte)
        }
        try Task.checkCancellation()
        return data
    }

    static func playableURL(_ output: ChatGeneratedOutput) async throws -> URL {
        guard let source = output.source else { throw Failure.unavailable }
        if !source.hasPrefix("data:"), let url = URL(string: source), ["https", "http"].contains(url.scheme) { return url }
        let bytes = try await data(output)
        return try await Task.detached(priority: .utility) {
            let directory = FileManager.default.temporaryDirectory.appendingPathComponent("CentaurGeneratedOutputs", isDirectory: true)
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            let extensions = ["audio/wav": "wav", "audio/mpeg": "mp3", "audio/mp4": "m4a", "video/mp4": "mp4", "text/html": "html", "text/csv": "csv", "application/pdf": "pdf", "application/json": "json", "image/svg+xml": "svg", "text/plain": "txt", "text/markdown": "md"]
            let suffix = extensions[output.mimeType ?? ""] ?? "bin"
            let folder = directory.appendingPathComponent(output.id, isDirectory: true)
            try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
            var name = String((output.title as NSString).lastPathComponent.prefix(120))
                .replacingOccurrences(of: "[^A-Za-z0-9 ._-]", with: "-", options: .regularExpression)
            if name.isEmpty || name == "." || name == ".." { name = "Generated file" }
            let url = (name as NSString).pathExtension.isEmpty ? folder.appendingPathComponent(name).appendingPathExtension(suffix) : folder.appendingPathComponent(name)
            if !FileManager.default.fileExists(atPath: url.path) { try bytes.write(to: url, options: .atomic) }
            return url
        }.value
    }
}
