import Foundation
import InboxCore
import NanocodexContext

/// The phone exposes its own account-scoped files. It does not advertise a shell
/// or access to another app's data. This actor keeps file IO off the UI actor.
public actor HandWorkspace {
    public nonisolated let id: String
    public nonisolated let name: String
    private let root: URL
    private let platform: String
    private nonisolated let messageContext: ContextQuery?

    public init(id: String, name: String, root: URL, platform: String = "ios", messageContext: ContextQuery? = nil) throws {
        guard id.range(of: #"^[A-Za-z0-9][A-Za-z0-9._:-]{0,122}$"#, options: .regularExpression) != nil,
              !name.isEmpty, name.utf8.count <= 128 else { throw HandFailure.invalidIdentity }
        self.id = id; self.name = name; self.platform = platform; self.messageContext = messageContext
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        self.root = root.standardizedFileURL.resolvingSymlinksInPath()
    }

    public nonisolated var catalog: JSON {
        func tool(_ name: String, _ description: String, properties: [String: JSON], required: [String], parallel: Bool = true) -> JSON {
            .object([
                "provider": .string("native"), "remote_name": .string(name),
                "definition": .object(["type": .string("function"), "name": .string(name), "description": .string(description), "strict": .bool(false),
                    "parameters": .object(["type": .string("object"), "properties": .object(properties), "required": .array(required.map(JSON.string)), "additionalProperties": .bool(false)])]),
                "parallel_safe": .bool(parallel), "timeout_ms": .number(15_000)
            ])
        }
        let path: JSON = .object(["type": .string("string"), "description": .string("Path relative to this device's /workspace. Use . for its root.")])
        let contextTools = messageContext == nil ? [] : HandContextTools.catalog { tool($0, $1, properties: $2, required: $3) }
        return .object([
            "type": .string("catalog"), "attachment_id": .string(id),
            "machines": .array([.object(["id": .string(id), "name": .string(name), "workspace": .string("/workspace"), "capabilities": .array((["native", "filesystem", platform == "ios" ? "background_limited" : "background"] + (messageContext == nil ? [] : ["message_context"])).map(JSON.string))])]),
            "tools": .array([
                tool("device_info", "Read this Hand's device, workspace, and availability. It connects automatically unless disabled; iOS background execution is limited and not guaranteed.", properties: [:], required: []),
                tool("list_files", "List files in this device's app workspace. No setup is needed. Other apps' files are not accessible.", properties: ["path": path], required: ["path"]),
                tool("read_file", "Read a UTF-8 file from this device's app workspace.", properties: ["path": path], required: ["path"]),
                tool("write_file", "Write a UTF-8 file in this device's app workspace. Creates parent folders and replaces the file atomically.", properties: ["path": path, "content": .object(["type": .string("string")])], required: ["path", "content"], parallel: false)
            ] + contextTools)
        ])
    }

    public func call(name: String, input: JSON) throws -> JSON {
        try Task.checkCancellation()
        guard case .object(let fields) = input else { throw HandFailure.invalidInput }
        if let messageContext, HandContextTools.names.contains(name) {
            return try HandContextTools.call(name: name, fields: fields, context: messageContext)
        }
        if name == "device_info" {
            guard fields.isEmpty else { throw HandFailure.invalidInput }
            return .object(["id": .string(id), "name": .string(self.name), "platform": .string(platform), "workspace": .string("/workspace"), "availability": .string(platform == "ios" ? "automatic while active and during iOS-granted background time; locked-device availability is not guaranteed" : "automatic in the background while the app is running and the Mac is awake")])
        }
        guard ["list_files", "read_file", "write_file"].contains(name),
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

    private func resolve(_ path: String) throws -> URL {
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
    case contextAccess(String)
    public var errorDescription: String? {
        switch self {
        case .invalidIdentity: return "Invalid Hand identity."
        case .invalidInput: return "Invalid device tool input."
        case .outsideWorkspace: return "This path is outside the device workspace."
        case .invalidFile: return "Use a regular UTF-8 file."
        case .protocolViolation: return "The Hand received an invalid protocol frame."
        case .connectionLost: return "The device Hand is reconnecting."
        case .contextAccess(let message): return message
        }
    }
}
