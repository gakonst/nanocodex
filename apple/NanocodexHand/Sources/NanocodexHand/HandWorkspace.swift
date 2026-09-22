import Foundation
import ImageIO
import UniformTypeIdentifiers
import InboxCore
import NanocodexContext

/// The phone exposes its own account-scoped files. It does not advertise a shell
/// or access to another app's data. This actor keeps file IO off the UI actor.
public actor HandWorkspace {
    public nonisolated let id: String
    public nonisolated let name: String
    private nonisolated let root: URL
    private let platform: String
    private nonisolated let messageContext: ContextQuery?
    private nonisolated let flipper: FlipperZeroBridge?
    private nonisolated let bluetooth: BluetoothLEBridge?

    public init(
        id: String,
        name: String,
        root: URL,
        platform: String = "ios",
        messageContext: ContextQuery? = nil,
        flipper: FlipperZeroBridge? = nil,
        bluetooth: BluetoothLEBridge? = nil
    ) throws {
        guard id.range(of: #"^[A-Za-z0-9][A-Za-z0-9._:-]{0,122}$"#, options: .regularExpression) != nil,
              !name.isEmpty, name.utf8.count <= 128 else { throw HandFailure.invalidIdentity }
        self.id = id; self.name = name; self.platform = platform; self.messageContext = messageContext
        self.flipper = flipper; self.bluetooth = bluetooth
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        self.root = root.standardizedFileURL.resolvingSymlinksInPath()
    }

    public nonisolated var catalog: JSON {
        func tool(
            _ name: String,
            _ description: String,
            properties: [String: JSON],
            required: [String],
            parallel: Bool = true,
            timeout: Double = 15_000
        ) -> JSON {
            .object([
                "provider": .string("native"), "remote_name": .string(name),
                "definition": .object(["type": .string("function"), "name": .string(name), "description": .string(description), "strict": .bool(false),
                    "parameters": .object(["type": .string("object"), "properties": .object(properties), "required": .array(required.map(JSON.string)), "additionalProperties": .bool(false)])]),
                "parallel_safe": .bool(parallel), "timeout_ms": .number(timeout)
            ])
        }
        let path: JSON = .object(["type": .string("string"), "description": .string("Path relative to this device's /workspace. Use . for its root.")])
        let contextTools = messageContext == nil ? [] : HandContextTools.catalog { tool($0, $1, properties: $2, required: $3) }
        let flipperTools = flipper == nil ? [] : FlipperZeroTools.catalog {
            tool($0, $1, properties: $2, required: $3, parallel: false, timeout: 180_000)
        }
        let bluetoothTools = bluetooth == nil ? [] : BluetoothLETools.catalog {
            tool($0, $1, properties: $2, required: $3, parallel: false, timeout: 180_000)
        }
        let personalTools = HandPersonalTools.available ? HandPersonalTools.catalog {
            tool($0, $1, properties: $2, required: $3)
        } : []
        let optionalCapabilities = (messageContext == nil ? [] : ["message_context"])
            + (flipper == nil ? [] : ["bluetooth", "flipper_zero"])
            + (bluetooth == nil ? [] : ["bluetooth_le", "gatt"])
            + (HandPersonalTools.available ? ["contacts", "photos", "location"] : [])
        return .object([
            "type": .string("catalog"), "attachment_id": .string(id),
            "capabilities": .array([.string("turn_metadata")]),
            "machines": .array([.object(["id": .string(id), "name": .string(name), "workspace": .string("/workspace"), "capabilities": .array((["native", "filesystem", platform == "ios" ? "background_limited" : "background"] + optionalCapabilities).map(JSON.string))])]),
            "tools": .array([
                tool("device_info", "Read this Hand's device, workspace, and availability. It connects automatically unless disabled; iOS background execution is limited and not guaranteed.", properties: [:], required: []),
                tool("list_files", "List files in this device's app workspace. No setup is needed. Other apps' files are not accessible.", properties: ["path": path], required: ["path"]),
                tool("view_image", "Inspect an image file in this device workspace as an oriented JPEG bounded to 2048 pixels and 512 KiB. Display with image(result.content[1]).", properties: ["path": path], required: ["path"]),
                tool("read_file", "Read a UTF-8 file from this device's app workspace.", properties: ["path": path], required: ["path"]),
                tool("write_file", "Write a UTF-8 file in this device's app workspace. Creates parent folders and replaces the file atomically.", properties: ["path": path, "content": .object(["type": .string("string")])], required: ["path", "content"], parallel: false)
            ] + contextTools + flipperTools + bluetoothTools + personalTools)
        ])
    }

    public func call(name: String, input: JSON) async throws -> JSON {
        try Task.checkCancellation()
        guard case .object(let fields) = input else { throw HandFailure.invalidInput }
        if HandPersonalTools.available, name == "read_photo" {
            let request = try PersonalToolRequest(fields, allowed: ["id"])
            let id = try request.text("id", required: true)!
            #if os(iOS)
            let data = try await IOSPhotoReader.read(id: id)
            try Task.checkCancellation()
            return try savePhoto(data, id: id)
            #else
            throw HandFailure.contextAccess("Personal device tools require iOS.")
            #endif
        }
        if HandPersonalTools.available, HandPersonalTools.names.contains(name) {
            return try await HandPersonalTools.call(name: name, fields: fields)
        }
        if let messageContext, HandContextTools.names.contains(name) {
            return try HandContextTools.call(name: name, fields: fields, context: messageContext)
        }
        if let flipper, FlipperZeroTools.names.contains(name) {
            do { return try await FlipperZeroTools.call(name: name, fields: fields, bridge: flipper) }
            catch let error as HandFailure { throw error }
            catch { throw HandFailure.flipper(error.localizedDescription) }
        }
        if let bluetooth, BluetoothLETools.names.contains(name) {
            do { return try await BluetoothLETools.call(name: name, fields: fields, bridge: bluetooth) }
            catch let error as HandFailure { throw error }
            catch { throw HandFailure.bluetooth(error.localizedDescription) }
        }
        if name == "device_info" {
            guard fields.isEmpty else { throw HandFailure.invalidInput }
            return .object(["id": .string(id), "name": .string(self.name), "platform": .string(platform), "workspace": .string("/workspace"), "app_version": .string(Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "unknown"), "app_build": .string(Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "unknown"), "availability": .string(platform == "ios" ? "automatic while active and during iOS-granted background time; locked-device availability is not guaranteed" : "automatic in the background while the app is running and the Mac is awake")])
        }
        guard ["list_files", "read_file", "write_file", "view_image"].contains(name),
              Set(fields.keys) == Set(name == "write_file" ? ["path", "content"] : ["path"]),
              case .string(let path) = fields["path"] else { throw HandFailure.invalidInput }
        let url = try resolve(path)
        switch name {
        case "list_files":
            let children = try FileManager.default.contentsOfDirectory(at: url, includingPropertiesForKeys: [.isDirectoryKey, .isSymbolicLinkKey], options: [.skipsHiddenFiles]).sorted { $0.lastPathComponent < $1.lastPathComponent }
            return .object(["entries": .array(try children.map { file in
                let values = try file.resourceValues(forKeys: [.isDirectoryKey, .isSymbolicLinkKey])
                return .object(["name": .string(file.lastPathComponent), "kind": .string(values.isSymbolicLink == true ? "link" : values.isDirectory == true ? "directory" : "file")])
            }), "has_more": .bool(false)])
        case "view_image":
            let data = try HandPhotoRendition.prepare(url: url)
            let size = try HandPhotoRendition.dimensions(data)
            let canonicalPath = "/workspace/" + String(url.path.dropFirst(root.path.count + 1))
            return try HandPhotoRendition.result(data, id: url.lastPathComponent, path: canonicalPath, width: size.0, height: size.1)
        case "read_file":
            let values = try url.resourceValues(forKeys: [.isRegularFileKey])
            guard values.isRegularFile == true else { throw HandFailure.invalidFile }
            let data = try Data(contentsOf: url)
            guard let text = String(data: data, encoding: .utf8) else { throw HandFailure.invalidFile }
            return .object(["content": .string(text)])
        default:
            guard case .string(let content) = fields["content"], url != root else { throw HandFailure.invalidFile }
            try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            try Task.checkCancellation()
            try Data(content.utf8).write(to: url, options: .atomic)
            return .object(["written_bytes": .number(Double(content.utf8.count))])
        }
    }

    /// Local UI access to retained attachments, including pending descriptors without a Hand ID.
    public nonisolated func localImageURL(attachment: MessageAttachment, preview: Bool) -> URL? {
        guard !attachment.isVideo, attachment.handID == nil || attachment.handID == id,
              UUID(uuidString: attachment.id) != nil else { return nil }
        let suffix = attachment.mediaType == "image/jpeg" ? "jpg" : String(attachment.mediaType.dropFirst("image/".count))
        guard suffix.range(of: #"^[a-z0-9][a-z0-9.+-]*$"#, options: .regularExpression) != nil else { return nil }
        let path = "/workspace/attachments/" + attachment.id.lowercased() + "/" + (preview ? "preview.jpg" : "original." + suffix)
        guard let url = try? resolve(path),
              let values = try? url.resourceValues(forKeys: [.isRegularFileKey, .isSymbolicLinkKey]),
              values.isRegularFile == true, values.isSymbolicLink != true else { return nil }
        return url
    }

    /// Publish both files as one directory rename; draft cleanup cannot remove them.
    /// Repeated publication keeps the first complete copy, including after source cleanup.
    public func publishImage(attachment: MessageAttachment, source: URL, preview: URL) async throws -> String {
        try Task.checkCancellation()
        guard !attachment.isVideo, attachment.handID == nil || attachment.handID == id, UUID(uuidString: attachment.id) != nil else { throw HandFailure.invalidInput }
        let suffix = attachment.mediaType == "image/jpeg" ? "jpg" : String(attachment.mediaType.dropFirst("image/".count))
        guard suffix.range(of: #"^[a-z0-9][a-z0-9.+-]*$"#, options: .regularExpression) != nil else { throw HandFailure.invalidInput }
        let directoryPath = "/workspace/attachments/" + attachment.id.lowercased()
        let path = directoryPath + "/original." + suffix
        let destination = try resolve(directoryPath)
        let original = try resolve(path)
        let retainedPreview = try resolve(directoryPath + "/preview.jpg")
        let manager = FileManager.default
        func validateOriginal(_ url: URL) throws {
            let values = try url.resourceValues(forKeys: [.isRegularFileKey, .isSymbolicLinkKey, .fileSizeKey])
            guard values.isRegularFile == true, values.isSymbolicLink != true, values.fileSize == attachment.byteCount,
                  let image = CGImageSourceCreateWithURL(url as CFURL, [kCGImageSourceShouldCache: false] as CFDictionary),
                  CGImageSourceGetCount(image) > 0,
                  let type = CGImageSourceGetType(image) as String?,
                  UTType(type)?.preferredMIMEType == attachment.mediaType else { throw HandFailure.invalidFile }
        }
        if manager.fileExists(atPath: destination.path) {
            try validateOriginal(original)
            _ = try HandPhotoRendition.dimensions(Data(contentsOf: retainedPreview))
            return path
        }
        guard source.isFileURL, preview.isFileURL else { throw HandFailure.invalidFile }
        let sourceAccess = source.startAccessingSecurityScopedResource()
        let previewAccess = preview.startAccessingSecurityScopedResource()
        defer {
            if sourceAccess { source.stopAccessingSecurityScopedResource() }
            if previewAccess { preview.stopAccessingSecurityScopedResource() }
        }
        let data = try HandPhotoRendition.prepare(url: preview)
        let parent = try resolve("/workspace/attachments")
        try manager.createDirectory(at: parent, withIntermediateDirectories: true)
        let staging = try resolve("/workspace/attachments/.publishing-" + UUID().uuidString)
        try manager.createDirectory(at: staging, withIntermediateDirectories: false)
        defer { try? manager.removeItem(at: staging) }
        let stagedOriginal = staging.appendingPathComponent("original." + suffix)
        try validateOriginal(source)
        try manager.copyItem(at: source, to: stagedOriginal)
        try validateOriginal(stagedOriginal)
        try data.write(to: staging.appendingPathComponent("preview.jpg"), options: .atomic)
        #if os(iOS)
        for url in [stagedOriginal, staging.appendingPathComponent("preview.jpg")] {
            try manager.setAttributes([.protectionKey: FileProtectionType.completeUntilFirstUserAuthentication], ofItemAtPath: url.path)
        }
        #endif
        try Task.checkCancellation()
        _ = try resolve(path)
        try manager.moveItem(at: staging, to: destination)
        return path
    }

    /// Materialize only a validated inspection rendition inside this account's workspace.
    func savePhoto(_ data: Data, id: String) throws -> JSON {
        let dimensions = try HandPhotoRendition.dimensions(data)
        let path = "/workspace/photos/" + UUID().uuidString.lowercased() + ".jpg"
        let destination = try resolve(path)
        try FileManager.default.createDirectory(at: destination.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Task.checkCancellation()
        try data.write(to: destination, options: .atomic)
        #if os(iOS)
        try FileManager.default.setAttributes([.protectionKey: FileProtectionType.completeUntilFirstUserAuthentication], ofItemAtPath: destination.path)
        #endif
        return try HandPhotoRendition.result(data, id: id, path: path, width: dimensions.0, height: dimensions.1)
    }

    private nonisolated func resolve(_ path: String) throws -> URL {
        guard !path.isEmpty, path.utf8.count <= 1024, !path.contains("\0"), !path.split(separator: "/").contains("..") else { throw HandFailure.outsideWorkspace }
        let relative: String
        if path == "/workspace" { relative = "." }
        else if path.hasPrefix("/workspace/") { relative = String(path.dropFirst(11)) }
        else if path.hasPrefix("/") { throw HandFailure.outsideWorkspace }
        else { relative = path }
        // Resolving a nonexistent leaf does not reliably resolve its parent
        // links. Check every existing component before creating or opening it.
        var componentURL = root
        for component in relative.split(separator: "/") where component != "." {
            componentURL.appendPathComponent(String(component))
            if (try? FileManager.default.destinationOfSymbolicLink(atPath: componentURL.path)) != nil { throw HandFailure.outsideWorkspace }
        }
        let candidate = root.appendingPathComponent(relative).standardizedFileURL
        let resolved = candidate.resolvingSymlinksInPath()
        guard (resolved == root || resolved.path.hasPrefix(root.path + "/")), resolved == candidate else { throw HandFailure.outsideWorkspace }
        return resolved
    }
}

public enum HandFailure: Error, LocalizedError {
    case invalidIdentity, invalidInput, outsideWorkspace, invalidFile, protocolViolation, connectionLost
    case contextAccess(String), flipper(String), bluetooth(String)
    public var errorDescription: String? {
        switch self {
        case .invalidIdentity: return "Invalid Hand identity."
        case .invalidInput: return "Invalid device tool input."
        case .outsideWorkspace: return "This path is outside the device workspace."
        case .invalidFile: return "Use a regular UTF-8 file."
        case .protocolViolation: return "The Hand received an invalid protocol frame."
        case .connectionLost: return "The device Hand is reconnecting."
        case .contextAccess(let message): return message
        case .flipper(let message): return message
        case .bluetooth(let message): return message
        }
    }
}
