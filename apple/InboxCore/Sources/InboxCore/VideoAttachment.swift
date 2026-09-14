import Foundation
import AVFoundation
import ImageIO
import CoreTransferable
import UniformTypeIdentifiers

public struct VideoAttachmentInfo: Codable, Equatable, Sendable {
    public let duration: Double
    public let timestamps: [Double]
    public let promptByteCount: Int
    public var original: Bool? = nil
    public var hasAudio: Bool? = nil
    func validate() throws {
        guard duration.isFinite, duration > 0, duration < Double(Int.max),
              (original == true ? timestamps.isEmpty : !timestamps.isEmpty),
              timestamps.allSatisfy({ $0.isFinite && $0 >= 0 && $0 <= duration }),
              zip(timestamps, timestamps.dropFirst()).allSatisfy({ $0.0 <= $0.1 }),
              promptByteCount > 0 else { throw AttachmentError.invalidReference }
    }
}

public struct TranscriptVideo: Identifiable, Codable, Equatable, Sendable {
    public let id: String
    public let name: String
    public let duration: Double
    public let timestamps: [Double]
    public let images: [String]
    public var path: String? = nil
    public var mediaType: String? = nil
    public var byteCount: Int? = nil
    public var hasAudio: Bool? = nil
}

public enum VideoAttachmentError: LocalizedError {
    case unsupported
    public var errorDescription: String? {
        switch self {
        case .unsupported: return "Choose a playable MP4 or MOV video."
        }
    }
}

/// Photos hands large movies over as files, never as an unbounded Data value.
public struct PickedVideo: Transferable, Sendable {
    public let url: URL
    public static var transferRepresentation: some TransferRepresentation {
        FileRepresentation(importedContentType: .movie) { received in
            let values = try received.file.resourceValues(forKeys: [.fileSizeKey, .isRegularFileKey])
            guard values.isRegularFile == true, let size = values.fileSize, size > 0 else { throw VideoAttachmentError.unsupported }
            let ext = received.file.pathExtension.lowercased() == "mp4" ? "mp4" : "mov"
            let copy = FileManager.default.temporaryDirectory.appendingPathComponent("picked-video-" + UUID().uuidString + "." + ext)
            try FileManager.default.copyItem(at: received.file, to: copy)
            return PickedVideo(url: copy)
        }
    }
}

public struct PreparedVideoAttachment: Sendable {
    public let attachment: MessageAttachment
    public let source: URL
    public let content: [JSON]
    public var poster: Data? = nil
}

public enum VideoAttachmentPreparation {
    public static func prepare(url: URL, name: String? = nil) async throws -> PreparedVideoAttachment {
        guard url.isFileURL, ["mov", "mp4", "m4v"].contains(url.pathExtension.lowercased()) else { throw VideoAttachmentError.unsupported }
        let access = url.startAccessingSecurityScopedResource()
        defer { if access { url.stopAccessingSecurityScopedResource() } }
        let values = try url.resourceValues(forKeys: [.isRegularFileKey, .fileSizeKey])
        guard values.isRegularFile == true, let bytes = values.fileSize else { throw VideoAttachmentError.unsupported }
        guard bytes > 0 else { throw VideoAttachmentError.unsupported }
        let asset = AVURLAsset(url: url)
        let protected = try await asset.load(.hasProtectedContent)
        let tracks = try await asset.loadTracks(withMediaType: .video)
        guard !protected, !tracks.isEmpty else { throw VideoAttachmentError.unsupported }
        let duration = try await asset.load(.duration).seconds
        guard duration.isFinite, duration > 0 else { throw VideoAttachmentError.unsupported }
        let hasAudio = !(try await asset.loadTracks(withMediaType: .audio)).isEmpty
        let generator = AVAssetImageGenerator(asset: asset)
        generator.appliesPreferredTrackTransform = true
        generator.maximumSize = CGSize(width: 640, height: 640)
        defer { generator.cancelAllCGImageGeneration() }
        let frame = try await generator.image(at: .zero)
        let poster = try jpeg(frame.image)
        try Task.checkCancellation()
        let id = UUID().uuidString
        let title = name ?? url.lastPathComponent
        let mediaType = url.pathExtension.lowercased() == "mov" ? "video/quicktime" : "video/mp4"
        let content = VideoAttachmentContent.original(id: id, name: title, duration: duration, mediaType: mediaType, byteCount: bytes, hasAudio: hasAudio)
        let encoded = try VideoAttachmentContent.encoder.encode(content)
        let info = VideoAttachmentInfo(duration: duration, timestamps: [], promptByteCount: encoded.count, original: true, hasAudio: hasAudio)
        let attachment = try MessageAttachment(id: id, name: title, mediaType: mediaType, byteCount: bytes, video: info)
        return PreparedVideoAttachment(attachment: attachment, source: url, content: content, poster: poster)
    }

    private static func jpeg(_ image: CGImage) throws -> Data {
        let bytes = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(bytes, "public.jpeg" as CFString, 1, nil) else { throw VideoAttachmentError.unsupported }
        CGImageDestinationAddImage(destination, image, [kCGImageDestinationLossyCompressionQuality: 0.82] as CFDictionary)
        guard CGImageDestinationFinalize(destination) else { throw VideoAttachmentError.unsupported }
        return bytes as Data
    }
}

/// Uses only the managed service's canonical text/image parts. Exact grouping
/// survives durable replay without requiring a new server input type.
enum VideoAttachmentContent {
    static let prefix = "Attached video for visual analysis using its sampled frames.\n[Video attachment v1]\n"
    static var encoder: JSONEncoder { let value = JSONEncoder(); value.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]; return value }
    static func label(_ time: Double) -> String { String(format: "Video frame at %.3f seconds", locale: Locale(identifier: "en_US_POSIX"), time) }
    static func make(id: String, name: String, duration: Double, timestamps: [Double], images: [Data]) -> [JSON] {
        let header: JSON = .object(["id": .string(id), "name": .string(name), "duration": .number(duration),
                                   "timestamps": .array(timestamps.map(JSON.number)), "audio_included": .bool(false)])
        let encoded = (try? encoder.encode(header)).map { String(decoding: $0, as: UTF8.self) } ?? ""
        var content: [JSON] = [.object(["type": .string("text"), "text": .string(prefix + encoded + "\nThese are sampled frames from the video in chronological order. Audio is not included; do not infer speech or unseen motion.")])]
        for (time, image) in zip(timestamps, images) {
            content.append(.object(["type": .string("text"), "text": .string(label(time))]))
            content.append(AttachmentPreparation.imageContent(image))
        }
        return content
    }
    static let originalPrefix = "Attached original video file.\n[Video attachment v2]\n"
    static func path(id: String, mediaType: String) -> String {
        "/brain/attachments/" + id.lowercased() + "/original." + (mediaType == "video/quicktime" ? "mov" : "mp4")
    }
    static func original(id: String, name: String, duration: Double, mediaType: String, byteCount: Int, hasAudio: Bool?) -> [JSON] {
        var values: [String: JSON] = ["id": .string(id), "name": .string(name), "duration": .number(duration),
            "path": .string(path(id: id, mediaType: mediaType)), "media_type": .string(mediaType), "size": .number(Double(byteCount))]
        if let hasAudio { values["has_audio"] = .bool(hasAudio) }
        let encoded = (try? encoder.encode(JSON.object(values))).map { String(decoding: $0, as: UTF8.self) } ?? ""
        return [.object(["type": .string("text"), "text": .string(originalPrefix + encoded + "\nThe original file is available at this filesystem path. Use tools to inspect it; its audio tracks are preserved.")])]
    }
    static func project(_ content: [JSON]) -> (videos: [TranscriptVideo], remaining: [JSON]) {
        var videos: [TranscriptVideo] = [], remaining: [JSON] = [], index = 0
        while index < content.count {
            let text = content[index]["text"].string
            if content[index]["type"].string == "text", text.hasPrefix(originalPrefix),
               let line = text.dropFirst(originalPrefix.count).split(separator: "\n").first,
               let header = try? JSONDecoder().decode(JSON.self, from: Data(line.utf8)),
               case .number(let size) = header["size"], size > 0, size < Double(Int.max), size.rounded(.down) == size,
               case .number(let duration) = header["duration"],
               header["has_audio"] == .null || header["has_audio"] == .bool(true) || header["has_audio"] == .bool(false) {
                let hasAudio: Bool? = header["has_audio"] == .null ? nil : header["has_audio"] == .bool(true)
                let info = VideoAttachmentInfo(duration: duration, timestamps: [], promptByteCount: 1, original: true, hasAudio: hasAudio)
                if let attachment = try? MessageAttachment(id: header["id"].string, name: header["name"].string,
                    mediaType: header["media_type"].string, byteCount: Int(size), video: info),
                   header["path"].string == path(id: attachment.id, mediaType: attachment.mediaType) {
                    videos.append(TranscriptVideo(id: attachment.id, name: attachment.name, duration: duration,
                        timestamps: [], images: [], path: header["path"].string, mediaType: attachment.mediaType, byteCount: attachment.byteCount, hasAudio: hasAudio))
                    index += 1; continue
                }
            }
            let marker = text.hasPrefix(prefix) ? prefix : "[Video attachment v1]\n"
            if content[index]["type"].string == "text", text.hasPrefix(marker),
               let line = text.dropFirst(marker.count).split(separator: "\n").first,
               let header = try? JSONDecoder().decode(JSON.self, from: Data(line.utf8)),
               MessageAttachment.validID(header["id"].string), !header["name"].string.isEmpty,
               header["audio_included"] == .bool(false),
               header["timestamps"].array.allSatisfy({ if case .number = $0 { return true }; return false }) {
                let times = header["timestamps"].array.map(\.number)
                let info = VideoAttachmentInfo(duration: header["duration"].number, timestamps: times, promptByteCount: 1)
                if (try? info.validate()) != nil, index + times.count * 2 < content.count {
                    var images: [String] = []
                    for (offset, time) in times.enumerated() {
                        let caption = content[index + 1 + offset * 2], image = content[index + 2 + offset * 2]
                        guard caption["type"].string == "text", caption["text"].string == label(time),
                              image["type"].string == "image", image["image_url"].string.hasPrefix(AttachmentPreparation.dataURLPrefix) else { break }
                        images.append(image["image_url"].string)
                    }
                    if images.count == times.count {
                        videos.append(TranscriptVideo(id: header["id"].string, name: header["name"].string, duration: info.duration, timestamps: times, images: images))
                        index += 1 + times.count * 2; continue
                    }
                }
            }
            remaining.append(content[index]); index += 1
        }
        return (videos, remaining)
    }
}

extension AttachmentStore {
    public func previewURL(for attachment: MessageAttachment) throws -> URL {
        let location = try fileURL(for: attachment.id)
        guard FileManager.default.fileExists(atPath: location.path) else { throw AttachmentError.unavailable }
        return location
    }
    public func save(_ prepared: PreparedVideoAttachment) throws {
        let attachment = prepared.attachment
        let data = try VideoAttachmentContent.encoder.encode(prepared.content)
        let frames = try validatedVideo(prepared.content, data: data, attachment: attachment)
        let poster = prepared.poster ?? frames.first.flatMap { Data(base64Encoded: String($0.dropFirst(AttachmentPreparation.dataURLPrefix.count))) }
        guard let poster else { throw AttachmentError.invalidReference }
        try AttachmentPreparation.validateJPEG(poster, attachment: MessageAttachment(name: "poster.jpg", byteCount: poster.count))
        let access = prepared.source.startAccessingSecurityScopedResource()
        defer { if access { prepared.source.stopAccessingSecurityScopedResource() } }
        guard prepared.source.isFileURL else { throw AttachmentError.invalidReference }
        let sourceValues = try prepared.source.resourceValues(forKeys: [.fileSizeKey, .isRegularFileKey])
        guard sourceValues.isRegularFile == true, sourceValues.fileSize == attachment.byteCount else { throw AttachmentError.invalidReference }
        let destination = try fileURL(for: attachment.id, extension: attachment.mediaType == "video/mp4" ? "mp4" : "mov")
        for ext in ["jpg", "mp4", "mov", "json"] {
            guard !FileManager.default.fileExists(atPath: try fileURL(for: attachment.id, extension: ext).path) else { throw AttachmentError.invalidReference }
        }
        try Task.checkCancellation()
        do {
            try FileManager.default.copyItem(at: prepared.source, to: destination)
            #if os(iOS)
            try FileManager.default.setAttributes([.protectionKey: FileProtectionType.completeUntilFirstUserAuthentication], ofItemAtPath: destination.path)
            #endif
            try poster.write(to: fileURL(for: attachment.id), options: .atomic)
            try data.write(to: fileURL(for: attachment.id, extension: "json"), options: .atomic)
            try Task.checkCancellation()
        } catch { try? remove(attachment); throw error }
    }
    func videoContent(for attachment: MessageAttachment) throws -> [JSON] {
        let location = try fileURL(for: attachment.id, extension: "json")
        guard let size = try location.resourceValues(forKeys: [.fileSizeKey]).fileSize,
              size == attachment.video?.promptByteCount else { throw AttachmentError.invalidReference }
        let data = try Data(contentsOf: location)
        let content = try JSONDecoder().decode([JSON].self, from: data)
        _ = try validatedVideo(content, data: data, attachment: attachment)
        return content
    }
    private func validatedVideo(_ content: [JSON], data: Data, attachment: MessageAttachment) throws -> [String] {
        guard let info = attachment.video, data.count == info.promptByteCount else { throw AttachmentError.invalidReference }
        let parsed = VideoAttachmentContent.project(content)
        guard parsed.remaining.isEmpty, parsed.videos.count == 1, let video = parsed.videos.first,
              video.id == attachment.id, video.name == attachment.name, video.duration == info.duration, video.timestamps == info.timestamps else { throw AttachmentError.invalidReference }
        if info.original == true {
            guard video.path == VideoAttachmentContent.path(id: attachment.id, mediaType: attachment.mediaType),
                  video.byteCount == attachment.byteCount, video.mediaType == attachment.mediaType,
                  video.hasAudio == info.hasAudio else { throw AttachmentError.invalidReference }
        } else if video.path != nil { throw AttachmentError.invalidReference }
        for image in video.images {
            guard let data = Data(base64Encoded: String(image.dropFirst(AttachmentPreparation.dataURLPrefix.count))) else { throw AttachmentError.invalidReference }
            try AttachmentPreparation.validateJPEG(data, attachment: MessageAttachment(name: "frame.jpg", byteCount: data.count))
        }
        return video.images
    }
}

public extension MessageAttachment {
    /// Use only after the managed service has confirmed the original upload.
    func originalVideoContent(path: String) throws -> [JSON] {
        guard let video, path == VideoAttachmentContent.path(id: id, mediaType: mediaType) else { throw AttachmentError.invalidReference }
        return VideoAttachmentContent.original(id: id, name: name, duration: video.duration, mediaType: mediaType, byteCount: byteCount, hasAudio: video.hasAudio)
    }
}
