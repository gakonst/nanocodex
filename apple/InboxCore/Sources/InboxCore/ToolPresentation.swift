import Foundation

/// Presentation of arbitrary tool payloads. Keep the wire envelope out of the conversation.
public struct ToolField: Codable, Equatable, Sendable {
    public var label: String
    public var value: String
    public var code: Bool = false
}

public struct ToolPresentation: Codable, Equatable, Sendable {
    public var title: String
    public var subject: String
    public var status: String
    public var input: [ToolField]
    public var output: [ToolField] = []

    public init(name: String, arguments: JSON, metadata: JSON = .null) {
        var family = metadata["tool_name"].string
        if family.isEmpty { family = metadata["toolName"].string }
        if family.isEmpty { family = name.hasPrefix("user_") ? "machine_action" : name }
        if family.hasPrefix("mcp__") { family = family.components(separatedBy: "__").dropFirst(2).joined(separator: "_") }
        if family.hasPrefix("functions.") { family = String(family.dropFirst(10)) }
        let names = [
            "exec": "Run code", "exec_command": "Run command", "sandbox_exec": "Run command",
            "read_file": "Read file", "write_file": "Write file", "apply_patch": "Edit files",
            "search_query": "Search the web", "web_search": "Search the web", "search": "Search",
            "browser_navigate": "Open page", "browser_execute": "Use browser", "browser_screenshot": "Capture page",
            "spawn_agent": "Delegate task", "wait_agent": "Wait for agent", "send_agent_message": "Message agent",
            "interrupt_agent": "Interrupt agent", "close_agent": "Close agent", "list_agents": "Check agents",
            "sandbox_start_process": "Start process", "sandbox_get_process": "Check process",
            "sandbox_kill_process": "Stop process", "sandbox_preview": "Open preview",
            "accountInfo": "Check account", "requestAccountConnection": "Connect account",
            "machine_action": "Use connected machine"
        ]
        title = names[family] ?? Self.humanize(family)
        let decoded = Self.decoded(arguments)
        subject = ["title", "description", "path", "file_path", "query", "url", "task", "command", "cmd"]
            .map { decoded[$0].string }.first(where: { !$0.isEmpty }) ?? ""
        subject = String(subject.split(whereSeparator: { $0.isWhitespace }).joined(separator: " ").prefix(140))
        input = Self.fields(decoded, label: "Input")
        status = "Running"
    }

    public mutating func finish(_ value: JSON, failed: Bool = false, state: String = "", metadata: JSON = .null) {
        let result = Self.decoded(value)
        let exitFailed: Bool
        if case .number(let code) = result["exit_code"] { exitFailed = code != 0 } else { exitFailed = false }
        let hasError = result["error"] != .null && result["error"] != .bool(false) && result["error"] != .string("")
        let isFailure = failed || state == "failed" || exitFailed || hasError || result["isError"].bool || result["is_error"].bool
        status = state == "cancelled" ? "Stopped" : isFailure ? "Failed" : "Completed"
        if !metadata["tool_name"].string.isEmpty || !metadata["toolName"].string.isEmpty {
            title = ToolPresentation(name: "", arguments: .null, metadata: metadata).title
        }
        output = Self.fields(result, label: "Result")
        if output.isEmpty { output = [.init(label: "Result", value: isFailure ? "The action failed without an error message." : "No output returned.")] }
    }

    public static func humanize(_ value: String) -> String {
        let spaced = value.replacingOccurrences(of: "([a-z0-9])([A-Z])", with: "$1 $2", options: .regularExpression)
            .replacingOccurrences(of: "[_./-]+", with: " ", options: .regularExpression)
            .split(whereSeparator: { $0.isWhitespace }).joined(separator: " ").lowercased()
        return spaced.isEmpty ? "Activity" : spaced.prefix(1).uppercased() + spaced.dropFirst()
    }

    public static func decoded(_ value: JSON) -> JSON {
        guard case .string(let text) = value, let first = text.trimmingCharacters(in: .whitespacesAndNewlines).first,
              first == "{" || first == "[", let json = try? JSONDecoder().decode(JSON.self, from: Data(text.utf8)) else { return value }
        return json
    }

    public static func fields(_ value: JSON, label: String) -> [ToolField] {
        let value = decoded(value)
        switch value {
        case .null: return []
        case .bool(let flag): return [.init(label: label, value: flag ? "Yes" : "No")]
        case .number(let number): return [.init(label: label, value: number.formatted(.number.grouping(.never)))]
        case .string(let text):
            guard !text.isEmpty else { return [] }
            if text.hasPrefix("data:") { return [.init(label: label, value: "Embedded attachment")] }
            return [.init(label: label, value: text, code: ["Command", "Code", "Output", "Error output", "Patch"].contains(label))]
        case .array(let items):
            return items.enumerated().flatMap { fields($0.element, label: items.count == 1 ? label : "\(label) · \($0.offset + 1)") }
        case .object(let object):
            // Binary content is described, never printed as base64 in a transcript.
            let kind = object["type"]?.string ?? ""
            if ["image", "audio", "input_audio", "image_url"].contains(kind) {
                return [.init(label: humanize(kind), value: object["name"]?.string.isEmpty == false ? object["name"]!.string : "\(humanize(kind)) attachment")]
            }
            if kind == "text", let text = object["text"] { return fields(text, label: label) }
            let labels = ["cmd": "Command", "command": "Command", "stdout": "Output", "stderr": "Error output",
                          "workdir": "Folder", "cwd": "Folder", "exit_code": "Exit code", "file_path": "File",
                          "is_error": "Failed", "isError": "Failed", "uri": "Location", "url": "Link"]
            return object.keys.sorted().flatMap { key -> [ToolField] in
                let field = labels[key] ?? humanize(key)
                let child = object[key]!
                if key == "data", object["mimeType"] != nil || object["mime_type"] != nil {
                    return [.init(label: "Attachment", value: "Embedded file")]
                }
                if case .object = decoded(child) {
                    return fields(child, label: field).map { .init(label: field + " · " + $0.label, value: $0.value, code: $0.code) }
                }
                return fields(child, label: field)
            }
        }
    }
}
