import Foundation
import InboxCore

enum FlipperZeroTools {
    static let names = Set([
        "flipper_devices", "flipper_connect", "flipper_disconnect", "flipper_info",
        "flipper_list_files", "flipper_read_file", "flipper_write_file",
        "flipper_start_app", "flipper_exit_app", "flipper_press_button", "flipper_screen"
    ])

    static func catalog(_ tool: (String, String, [String: JSON], [String]) -> JSON) -> [JSON] {
        func string(_ description: String, values: [String]? = nil) -> JSON {
            var schema: [String: JSON] = ["type": .string("string"), "description": .string(description)]
            if let values { schema["enum"] = .array(values.map(JSON.string)) }
            return .object(schema)
        }
        let flipperPath = string("Absolute path on the Flipper, normally under /ext or /int. Parent traversal is rejected.")
        return [
            tool("flipper_devices", "Scan nearby Bluetooth LE devices for Flipper Zero. This may show the system Bluetooth permission prompt. Pairing happens only when connecting.", [:], []),
            tool("flipper_connect", "Connect or reconnect this phone to a Flipper Zero over encrypted Bluetooth LE RPC. Omit device_id to use the last paired Flipper or the only nearby device. iOS may ask the user to confirm the pairing code shown on the Flipper.", [
                "device_id": string("Device ID returned by flipper_devices. Omit when there is only one Flipper or a previously paired device.")
            ], []),
            tool("flipper_disconnect", "Disconnect the Flipper Zero. Set forget to remove the remembered device choice; iOS Bluetooth pairing records remain controlled by Settings.", [
                "forget": .object(["type": .string("boolean"), "description": .string("Forget Nanocodex's last-device choice. Defaults to false.")])
            ], []),
            tool("flipper_info", "Read the connected Flipper Zero's firmware, hardware, protobuf, battery, and Bluetooth information. Automatically reconnects the last paired device.", [:], []),
            tool("flipper_list_files", "List files and directories on the connected Flipper Zero over Bluetooth RPC.", [
                "path": flipperPath,
                "limit": .object(["type": .string("integer"), "minimum": .number(1), "description": .string("Maximum entries returned to the agent. Omit to return the complete directory.")])
            ], ["path"]),
            tool("flipper_read_file", "Read a file from the connected Flipper Zero. UTF-8 files return text; other files return base64. Use max_bytes when the file may be large.", [
                "path": flipperPath,
                "max_bytes": .object(["type": .string("integer"), "minimum": .number(1), "description": .string("Fail without returning content if the response exceeds this byte budget. Omit for no tool-level limit.")])
            ], ["path"]),
            tool("flipper_write_file", "Create or replace a file on the connected Flipper Zero. Supply exactly one of content (UTF-8) or base64. Data is chunked using Flipper's official 512-byte RPC write contract.", [
                "path": flipperPath,
                "content": string("UTF-8 file content."),
                "base64": string("Binary file content encoded as base64.")
            ], ["path"]),
            tool("flipper_start_app", "Start an installed application on the connected Flipper Zero by its firmware application name.", [
                "name": string("Firmware application name."),
                "args": string("Optional application arguments.")
            ], ["name"]),
            tool("flipper_exit_app", "Exit the application currently running on the connected Flipper Zero.", [:], []),
            tool("flipper_press_button", "Press a physical navigation button through Flipper's GUI RPC. Use flipper_screen before and after when navigating an unfamiliar app.", [
                "key": string("Navigation button.", values: ["up", "down", "left", "right", "ok", "back"]),
                "long": .object(["type": .string("boolean"), "description": .string("Send a long press instead of a short press. Defaults to false.")])
            ], ["key"]),
            tool("flipper_screen", "Capture the current 128×64 Flipper display over Bluetooth RPC. Returns the raw framebuffer plus a compact braille preview readable by the agent.", [:], [])
        ]
    }

    @MainActor
    static func call(name: String, fields: [String: JSON], bridge: FlipperZeroBridge) async throws -> JSON {
        switch name {
        case "flipper_devices":
            try exact(fields, allowed: [])
            let devices = try await bridge.devices()
            return .object(["state": .string(bridge.state), "devices": .array(devices.map(deviceJSON))])
        case "flipper_connect":
            try exact(fields, allowed: ["device_id"])
            let id = try optionalString("device_id", in: fields, maximum: 64)
            return .object(["state": .string("connected"), "device": deviceJSON(try await bridge.connect(deviceID: id))])
        case "flipper_disconnect":
            try exact(fields, allowed: ["forget"])
            let forget = try optionalBool("forget", in: fields) ?? false
            bridge.disconnect(forget: forget)
            return .object(["state": .string("disconnected"), "forgot_device": .bool(forget)])
        case "flipper_info":
            try exact(fields, allowed: [])
            let info = try await bridge.info()
            var rpc: [String: JSON] = [:]
            for (key, value) in info.rpc { rpc[key] = .string(value) }
            return .object([
                "device": deviceJSON(info.device), "battery_percent": info.batteryPercent.map { .number(Double($0)) } ?? .null,
                "bluetooth": .object(info.bluetooth.mapValues(JSON.string)), "rpc": .object(rpc)
            ])
        case "flipper_list_files":
            try exact(fields, allowed: ["path", "limit"])
            let path = try requiredPath(in: fields)
            let limit = try optionalInteger("limit", in: fields)
            let files = try await bridge.list(path: path)
            let selected = limit.map { Array(files.prefix($0)) } ?? files
            return .object([
                "path": .string(path),
                "entries": .array(selected.map { file in
                    .object([
                        "name": .string(file.name), "kind": .string(file.isDirectory ? "directory" : "file"),
                        "size": .number(Double(file.size)), "md5": file.md5.map(JSON.string) ?? .null
                    ])
                }),
                "total_entries": .number(Double(files.count)), "truncated": .bool(selected.count != files.count)
            ])
        case "flipper_read_file":
            try exact(fields, allowed: ["path", "max_bytes"])
            let path = try requiredPath(in: fields)
            let maximum = try optionalInteger("max_bytes", in: fields)
            let data = try await bridge.read(path: path, maximumBytes: maximum)
            var result: [String: JSON] = ["path": .string(path), "size": .number(Double(data.count))]
            if let text = String(data: data, encoding: .utf8) {
                result["encoding"] = .string("utf8")
                result["content"] = .string(text)
            } else {
                result["encoding"] = .string("base64")
                result["content"] = .string(data.base64EncodedString())
            }
            return .object(result)
        case "flipper_write_file":
            try exact(fields, allowed: ["path", "content", "base64"])
            let path = try requiredPath(in: fields)
            let content = try optionalString("content", in: fields)
            let base64 = try optionalString("base64", in: fields)
            guard (content == nil) != (base64 == nil) else { throw HandFailure.invalidInput }
            let data: Data
            if let content { data = Data(content.utf8) }
            else if let base64, let decoded = Data(base64Encoded: base64) { data = decoded }
            else { throw HandFailure.invalidInput }
            try await bridge.write(path: path, data: data)
            return .object(["path": .string(path), "written_bytes": .number(Double(data.count))])
        case "flipper_start_app":
            try exact(fields, allowed: ["name", "args"])
            let app = try requiredString("name", in: fields, maximum: 128)
            let args = try optionalString("args", in: fields, maximum: 1_024) ?? ""
            try await bridge.startApp(name: app, args: args)
            return .object(["started": .string(app), "args": .string(args)])
        case "flipper_exit_app":
            try exact(fields, allowed: [])
            try await bridge.exitApp()
            return .object(["exited": .bool(true)])
        case "flipper_press_button":
            try exact(fields, allowed: ["key", "long"])
            let key = try requiredString("key", in: fields, maximum: 5)
            let keys: [String: UInt64] = ["up": 0, "down": 1, "right": 2, "left": 3, "ok": 4, "back": 5]
            guard let code = keys[key] else { throw HandFailure.invalidInput }
            let long = try optionalBool("long", in: fields) ?? false
            try await bridge.press(key: code, long: long)
            return .object(["key": .string(key), "press": .string(long ? "long" : "short")])
        case "flipper_screen":
            try exact(fields, allowed: [])
            let screen = try await bridge.screen()
            let orientations = ["horizontal", "horizontal_flipped", "vertical", "vertical_flipped"]
            let orientation = screen.orientation < UInt64(orientations.count) ? orientations[Int(screen.orientation)] : "unknown"
            return .object([
                "width": .number(128), "height": .number(64), "orientation": .string(orientation),
                "preview": .string(screen.braille), "framebuffer_base64": .string(screen.bytes.base64EncodedString())
            ])
        default:
            throw HandFailure.invalidInput
        }
    }

    private static func deviceJSON(_ device: FlipperZeroDevice) -> JSON {
        .object([
            "device_id": .string(device.id), "name": .string(device.name), "color": .string(device.color),
            "rssi": device.rssi.map { .number(Double($0)) } ?? .null, "connected": .bool(device.connected)
        ])
    }

    private static func exact(_ fields: [String: JSON], allowed: Set<String>) throws {
        guard Set(fields.keys).isSubset(of: allowed) else { throw HandFailure.invalidInput }
    }

    private static func requiredPath(in fields: [String: JSON]) throws -> String {
        let path = try requiredString("path", in: fields, maximum: 1_024)
        guard path.hasPrefix("/"), !path.contains("\0"), !path.split(separator: "/").contains("..") else {
            throw HandFailure.invalidInput
        }
        return path
    }

    private static func requiredString(_ key: String, in fields: [String: JSON], maximum: Int) throws -> String {
        guard let value = try optionalString(key, in: fields, maximum: maximum), !value.isEmpty else {
            throw HandFailure.invalidInput
        }
        return value
    }

    private static func optionalString(_ key: String, in fields: [String: JSON], maximum: Int? = nil) throws -> String? {
        guard let value = fields[key] else { return nil }
        guard case let .string(text) = value, maximum.map({ text.utf8.count <= $0 }) ?? true else {
            throw HandFailure.invalidInput
        }
        return text
    }

    private static func optionalBool(_ key: String, in fields: [String: JSON]) throws -> Bool? {
        guard let value = fields[key] else { return nil }
        guard case let .bool(flag) = value else { throw HandFailure.invalidInput }
        return flag
    }

    private static func optionalInteger(_ key: String, in fields: [String: JSON]) throws -> Int? {
        guard let value = fields[key] else { return nil }
        guard case let .number(number) = value, number.isFinite, number >= 1,
              number <= Double(Int.max), number.rounded() == number else { throw HandFailure.invalidInput }
        return Int(number)
    }
}
