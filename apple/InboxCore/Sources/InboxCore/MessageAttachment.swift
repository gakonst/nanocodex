import Foundation
import Darwin
import UniformTypeIdentifiers
import CoreTransferable
#if canImport(ImageIO) && canImport(CoreGraphics)
import ImageIO
import CoreGraphics
#endif

public struct MessageAttachment: Identifiable, Codable, Equatable, Sendable {
    public let id: String
    public let name: String
    public let mediaType: String
    public let byteCount: Int
    public let video: VideoAttachmentInfo?
    public var isVideo: Bool { video != nil }
    public var promptByteCount: Int { video?.promptByteCount ?? ((try? JSONEncoder().encode(ImageAttachmentContent.original(self)).count) ?? 0) }
    public var originalPath: String {
        isVideo ? VideoAttachmentContent.path(id: id, mediaType: mediaType) : ImageAttachmentContent.path(id: id, mediaType: mediaType)
    }

    public init(id: String = UUID().uuidString, name: String, mediaType: String = "image/jpeg", byteCount: Int, video: VideoAttachmentInfo? = nil) throws {
        guard Self.validID(id), !name.isEmpty,
              !name.contains("/"), !name.contains("\\"), name != ".", name != "..",
              !name.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains),
              (video == nil ? mediaType.range(of: #"^image/[a-z0-9][a-z0-9.+-]*$"#, options: .regularExpression) != nil && byteCount > 0
               : ["video/mp4", "video/quicktime"].contains(mediaType) && byteCount > 0) else {
            throw AttachmentError.invalidReference
        }
        try video?.validate()
        self.id = id; self.name = name; self.mediaType = mediaType; self.byteCount = byteCount; self.video = video
    }

    public init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        try self.init(id: values.decode(String.self, forKey: .id), name: values.decode(String.self, forKey: .name),
                      mediaType: values.decode(String.self, forKey: .mediaType), byteCount: values.decode(Int.self, forKey: .byteCount),
                      video: values.decodeIfPresent(VideoAttachmentInfo.self, forKey: .video))
    }

    static func validID(_ id: String) -> Bool {
        UUID(uuidString: id)?.uuidString.lowercased() == id.lowercased()
    }
}

/// Photos provides original image files without materializing them as Data.
public struct PickedImage: Transferable, Sendable {
    public let url: URL
    public static var transferRepresentation: some TransferRepresentation {
        FileRepresentation(importedContentType: .image) { received in
            let copy = FileManager.default.temporaryDirectory.appendingPathComponent("picked-image-" + UUID().uuidString + "." + received.file.pathExtension)
            try FileManager.default.copyItem(at: received.file, to: copy)
            return PickedImage(url: copy)
        }
    }
}

public struct PreparedAttachment: Sendable {
    public enum Source: Sendable { case file(URL), data(Data) }
    public let attachment: MessageAttachment
    public let source: Source
    public let preview: Data
    public var content: [JSON] { ImageAttachmentContent.original(attachment) }
    public init(attachment: MessageAttachment, source: Source, preview: Data) {
        self.attachment = attachment; self.source = source; self.preview = preview
    }
}

public enum AttachmentError: Error, LocalizedError, Equatable, Sendable {
    case unsupportedImage, invalidReference, invalidScope, unavailable
    public var errorDescription: String? {
        switch self {
        case .unsupportedImage: return "Choose a supported image, such as a JPEG, PNG, or HEIC photo."
        case .invalidReference: return "This attachment is invalid. Remove it and add it again."
        case .invalidScope: return "Sign in before attaching media."
        case .unavailable: return "This attachment is no longer available. Remove it and add it again."
        }
    }
}

public enum AttachmentPreparation {
    static let dataURLPrefix = "data:image/jpeg;base64,"

    public static func prepare(url: URL, name: String? = nil) throws -> PreparedAttachment {
        guard url.isFileURL else { throw AttachmentError.unsupportedImage }
        let access = url.startAccessingSecurityScopedResource()
        defer { if access { url.stopAccessingSecurityScopedResource() } }
        let values = try url.resourceValues(forKeys: [.isRegularFileKey, .fileSizeKey])
        guard values.isRegularFile == true, let count = values.fileSize, count > 0,
              let image = CGImageSourceCreateWithURL(url as CFURL, [kCGImageSourceShouldCache: false] as CFDictionary)
        else { throw AttachmentError.unsupportedImage }
        return try prepare(image: image, source: .file(url), name: name ?? url.lastPathComponent, byteCount: count)
    }

    public static func prepare(data: Data, name: String, mediaType: String) throws -> PreparedAttachment {
        guard !data.isEmpty, mediaType.lowercased().hasPrefix("image/"),
              let image = CGImageSourceCreateWithData(data as CFData, [kCGImageSourceShouldCache: false] as CFDictionary)
        else { throw AttachmentError.unsupportedImage }
        return try prepare(image: image, source: .data(data), name: name, byteCount: data.count)
    }

    private static func prepare(image: CGImageSource, source: PreparedAttachment.Source, name: String, byteCount: Int) throws -> PreparedAttachment {
        guard CGImageSourceGetCount(image) > 0, let identifier = CGImageSourceGetType(image) as String?,
              let mediaType = UTType(identifier)?.preferredMIMEType,
              let thumbnail = CGImageSourceCreateThumbnailAtIndex(image, 0, [
                kCGImageSourceCreateThumbnailFromImageAlways: true,
                kCGImageSourceCreateThumbnailWithTransform: true,
                kCGImageSourceThumbnailMaxPixelSize: 640,
                kCGImageSourceShouldCacheImmediately: true
              ] as CFDictionary) else { throw AttachmentError.unsupportedImage }
        let bytes = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(bytes, "public.jpeg" as CFString, 1, nil) else { throw AttachmentError.unsupportedImage }
        CGImageDestinationAddImage(destination, thumbnail, [kCGImageDestinationLossyCompressionQuality: 0.82] as CFDictionary)
        guard CGImageDestinationFinalize(destination) else { throw AttachmentError.unsupportedImage }
        return PreparedAttachment(attachment: try MessageAttachment(name: name, mediaType: mediaType, byteCount: byteCount),
                                  source: source, preview: bytes as Data)
    }

    static func imageContent(_ data: Data) -> JSON {
        .object(["type": .string("image"), "image_url": .string(dataURLPrefix + data.base64EncodedString()), "detail": .string("high")])
    }

    static func validateJPEG(_ data: Data, attachment: MessageAttachment) throws {
        guard data.count == attachment.byteCount,
              let source = CGImageSourceCreateWithData(data as CFData, [kCGImageSourceShouldCache: false] as CFDictionary),
              CGImageSourceGetType(source) as String? == "public.jpeg", CGImageSourceGetCount(source) > 0 else { throw AttachmentError.invalidReference }
    }
}

/// Originals remain files. Only small path records enter turns and history.
public struct AttachmentStore: Sendable {
    let directory: URL

    public init(scope: String) throws {
        let support = try FileManager.default.url(for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true)
        try self.init(scope: scope, rootDirectory: support.appendingPathComponent("InboxAttachments", isDirectory: true))
    }

    init(scope: String, rootDirectory: URL) throws {
        let allowed = CharacterSet(charactersIn: "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._-")
        guard !scope.isEmpty, scope.utf8.count <= 128, !scope.hasPrefix("."), !scope.contains(".."),
              scope.unicodeScalars.allSatisfy(allowed.contains) else { throw AttachmentError.invalidScope }
        directory = rootDirectory.appendingPathComponent(scope, isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        guard try directory.resourceValues(forKeys: [.isSymbolicLinkKey]).isSymbolicLink != true else { throw AttachmentError.invalidScope }
        var excluded = directory
        var values = URLResourceValues(); values.isExcludedFromBackup = true
        try excluded.setResourceValues(values)
        try migrateOriginalImages()
    }

    /// Upgrade old JPEG-only drafts once; all reads then use the original-file layout.
    private func migrateOriginalImages() throws {
        let manager = FileManager.default
        let marker = directory.appendingPathComponent(".original-images-v1")
        guard !manager.fileExists(atPath: marker.path) else { return }
        let lock = open(directory.appendingPathComponent(".migration.lock").path, O_CREAT | O_RDWR | O_CLOEXEC | O_NOFOLLOW, 0o600)
        guard lock >= 0 else { throw AttachmentError.unavailable }
        defer { close(lock) }
        guard flock(lock, LOCK_EX) == 0 else { throw AttachmentError.unavailable }
        defer { flock(lock, LOCK_UN) }
        guard !manager.fileExists(atPath: marker.path) else { return }
        for file in try manager.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil) where file.pathExtension == "jpg" {
            let id = file.deletingPathExtension().lastPathComponent
            guard MessageAttachment.validID(id) else { continue }
            let destination = try fileURL(for: id, extension: "original")
            guard !manager.fileExists(atPath: destination.path),
                  !["mp4", "mov", "json"].contains(where: { manager.fileExists(atPath: directory.appendingPathComponent(id + "." + $0).path) }) else { continue }
            let temporary = destination.appendingPathExtension("migrating")
            if manager.fileExists(atPath: temporary.path) { try manager.removeItem(at: temporary) }
            defer { try? manager.removeItem(at: temporary) }
            try manager.copyItem(at: fileURL(for: id), to: temporary)
            try manager.moveItem(at: temporary, to: destination)
        }
        try Data().write(to: marker, options: .atomic)
    }

    public func save(_ prepared: PreparedAttachment) throws {
        let attachment = prepared.attachment
        guard !attachment.isVideo else { throw AttachmentError.invalidReference }
        let destination = try fileURL(for: attachment.id, extension: "original")
        let preview = try fileURL(for: attachment.id)
        guard !FileManager.default.fileExists(atPath: destination.path), !FileManager.default.fileExists(atPath: preview.path) else { throw AttachmentError.invalidReference }
        try AttachmentPreparation.validateJPEG(prepared.preview, attachment: MessageAttachment(name: "preview.jpg", byteCount: prepared.preview.count))
        do {
            switch prepared.source {
            case .file(let source):
                let access = source.startAccessingSecurityScopedResource()
                defer { if access { source.stopAccessingSecurityScopedResource() } }
                guard source.isFileURL else { throw AttachmentError.invalidReference }
                try FileManager.default.copyItem(at: source, to: destination)
            case .data(let data): try data.write(to: destination, options: .atomic)
            }
            try validateImage(at: destination, attachment: attachment)
            #if os(iOS)
            try FileManager.default.setAttributes([.protectionKey: FileProtectionType.completeUntilFirstUserAuthentication], ofItemAtPath: destination.path)
            #endif
            try prepared.preview.write(to: preview, options: .atomic)
        } catch { try? remove(attachment); throw error }
    }

    private func validateImage(at url: URL, attachment: MessageAttachment) throws {
        guard try url.resourceValues(forKeys: [.fileSizeKey]).fileSize == attachment.byteCount,
              let source = CGImageSourceCreateWithURL(url as CFURL, [kCGImageSourceShouldCache: false] as CFDictionary),
              let identifier = CGImageSourceGetType(source) as String?,
              UTType(identifier)?.preferredMIMEType == attachment.mediaType,
              CGImageSourceGetCount(source) > 0 else { throw AttachmentError.invalidReference }
    }

    public func content(for attachments: [MessageAttachment]) throws -> [JSON] {
        try attachments.flatMap { attachment -> [JSON] in
            if attachment.isVideo { return try videoContent(for: attachment) }
            let location = try url(for: attachment)
            try validateImage(at: location, attachment: attachment)
            return ImageAttachmentContent.original(attachment)
        }
    }

    public func url(for attachment: MessageAttachment) throws -> URL {
        let location = try fileURL(for: attachment.id, extension: attachment.isVideo ? attachment.mediaType == "video/mp4" ? "mp4" : "mov" : "original")
        guard FileManager.default.fileExists(atPath: location.path) else { throw AttachmentError.unavailable }
        return location
    }

    public func remove(_ attachment: MessageAttachment) throws {
        for ext in attachment.isVideo ? ["jpg", "mp4", "mov", "json", "original"] : ["jpg", "original"] {
            let location = try fileURL(for: attachment.id, extension: ext)
            if FileManager.default.fileExists(atPath: location.path) { try FileManager.default.removeItem(at: location) }
        }
    }

    public func prune(keeping ids: Set<String>) throws {
        guard ids.allSatisfy(MessageAttachment.validID) else { throw AttachmentError.invalidReference }
        let retained = Set(ids.map { $0.lowercased() })
        for file in try FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil) {
            let id = file.deletingPathExtension().lastPathComponent
            guard ["jpg", "mp4", "mov", "json", "original"].contains(file.pathExtension), MessageAttachment.validID(id), !retained.contains(id.lowercased()) else { continue }
            try FileManager.default.removeItem(at: try fileURL(for: id, extension: file.pathExtension))
        }
    }

    func fileURL(for id: String, extension ext: String = "jpg") throws -> URL {
        guard MessageAttachment.validID(id), ["jpg", "mp4", "mov", "json", "original"].contains(ext) else { throw AttachmentError.invalidReference }
        let location = directory.appendingPathComponent(id.lowercased() + "." + ext, isDirectory: false)
        if FileManager.default.fileExists(atPath: location.path),
           try location.resourceValues(forKeys: [.isSymbolicLinkKey, .isRegularFileKey]).isRegularFile != true {
            throw AttachmentError.invalidReference
        }
        if (try? location.resourceValues(forKeys: [.isSymbolicLinkKey]).isSymbolicLink) == true { throw AttachmentError.invalidReference }
        return location
    }
}
