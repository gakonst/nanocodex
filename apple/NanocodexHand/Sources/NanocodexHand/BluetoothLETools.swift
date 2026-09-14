import Foundation
import InboxCore

enum BluetoothLETools {
    static let names = Set([
        "bluetooth_devices", "bluetooth_connect", "bluetooth_disconnect", "bluetooth_services",
        "bluetooth_read", "bluetooth_write", "bluetooth_subscribe", "bluetooth_collect"
    ])

    static func catalog(_ tool: (String, String, [String: JSON], [String]) -> JSON) -> [JSON] {
        func string(_ description: String) -> JSON { .object(["type": .string("string"), "description": .string(description)]) }
        let service = string("BLE service UUID, in 16-bit or full 128-bit form.")
        let characteristic = string("BLE characteristic UUID, in 16-bit or full 128-bit form.")
        let duration: JSON = .object(["type": .string("number"), "minimum": .number(0.1), "maximum": .number(120),
                                      "description": .string("Finite collection window in seconds; the tool must return before its 180-second call deadline.")])
        return [
            tool("bluetooth_devices", "Scan for nearby Bluetooth Low Energy peripherals. Optionally filter by advertised GATT services or local-name prefix. This does not cover Bluetooth Classic or OS-owned profiles.", [
                "service_uuids": .object(["type": .string("array"), "items": service]),
                "name_prefix": string("Optional case-insensitive advertised-name prefix."),
                "scan_seconds": .object(["type": .string("number"), "minimum": .number(0.5), "maximum": .number(10)])
            ], []),
            tool("bluetooth_connect", "Connect to a BLE peripheral and discover its GATT services and characteristics. device_id must come from bluetooth_devices.", [
                "device_id": string("CoreBluetooth device ID returned by bluetooth_devices."),
                "service_uuids": .object(["type": .string("array"), "items": service, "description": .string("Optional services to discover; omit to discover all services.")])
            ], ["device_id"]),
            tool("bluetooth_disconnect", "Disconnect the generic BLE peripheral.", [:], []),
            tool("bluetooth_services", "List the connected peripheral's discovered GATT characteristics and their read, write, notify, and indicate properties.", [:], []),
            tool("bluetooth_read", "Read one readable GATT characteristic. Returns base64 and UTF-8 when valid.", [
                "service_uuid": service, "characteristic_uuid": characteristic
            ], ["service_uuid", "characteristic_uuid"]),
            tool("bluetooth_write", "Write one GATT characteristic value. Supply exactly one of utf8, hex, or base64. The value must fit the peripheral's negotiated single-write length.", [
                "service_uuid": service, "characteristic_uuid": characteristic,
                "utf8": string("Text value encoded as UTF-8."), "hex": string("Binary value as an even-length hexadecimal string."),
                "base64": string("Binary value encoded as base64."),
                "with_response": .object(["type": .string("boolean"), "description": .string("Request an acknowledged write. Omit to prefer it when supported.")])
            ], ["service_uuid", "characteristic_uuid"]),
            tool("bluetooth_subscribe", "Enable or disable notifications or indications for a GATT characteristic.", [
                "service_uuid": service, "characteristic_uuid": characteristic,
                "enabled": .object(["type": .string("boolean")])
            ], ["service_uuid", "characteristic_uuid", "enabled"]),
            tool("bluetooth_collect", "Collect notifications already enabled with bluetooth_subscribe during a finite window. Omit UUID filters to collect every enabled characteristic.", [
                "duration_seconds": duration, "service_uuid": service, "characteristic_uuid": characteristic
            ], ["duration_seconds"])
        ]
    }

    @MainActor
    static func call(name: String, fields: [String: JSON], bridge: BluetoothLEBridge) async throws -> JSON {
        switch name {
        case "bluetooth_devices":
            try exact(fields, allowed: ["service_uuids", "name_prefix", "scan_seconds"])
            let services = try optionalStrings("service_uuids", in: fields)
            let prefix = try optionalString("name_prefix", in: fields, maximum: 128)
            let seconds = try optionalNumber("scan_seconds", in: fields, range: 0.5...10) ?? 3
            let devices = try await bridge.devices(serviceUUIDs: services, namePrefix: prefix, scanFor: seconds)
            return .object(["state": .string(bridge.state), "devices": .array(devices.map(deviceJSON))])
        case "bluetooth_connect":
            try exact(fields, allowed: ["device_id", "service_uuids"])
            let id = try requiredString("device_id", in: fields, maximum: 64)
            let services = fields["service_uuids"] == nil ? nil : try optionalStrings("service_uuids", in: fields)
            return .object(["state": .string("connected"), "device": deviceJSON(try await bridge.connect(deviceID: id, serviceUUIDs: services))])
        case "bluetooth_disconnect":
            try exact(fields, allowed: [])
            bridge.disconnect()
            return .object(["state": .string("disconnected")])
        case "bluetooth_services":
            try exact(fields, allowed: [])
            return .object(["characteristics": .array(try bridge.services().map { item in .object([
                "service_uuid": .string(item.serviceUUID), "characteristic_uuid": .string(item.characteristicUUID),
                "properties": .array(item.properties.map(JSON.string)), "notifying": .bool(item.notifying)
            ]) })])
        case "bluetooth_read":
            try exact(fields, allowed: ["service_uuid", "characteristic_uuid"])
            let (service, characteristic) = try uuids(fields)
            let value = try await bridge.read(serviceUUID: service, characteristicUUID: characteristic)
            return valueJSON(value)
        case "bluetooth_write":
            try exact(fields, allowed: ["service_uuid", "characteristic_uuid", "utf8", "hex", "base64", "with_response"])
            let (service, characteristic) = try uuids(fields)
            let utf8 = try optionalString("utf8", in: fields), hex = try optionalString("hex", in: fields), base64 = try optionalString("base64", in: fields)
            guard [utf8, hex, base64].compactMap({ $0 }).count == 1 else { throw HandFailure.invalidInput }
            let value: Data
            if let utf8 { value = Data(utf8.utf8) }
            else if let hex { value = try hexData(hex) }
            else if let base64, let decoded = Data(base64Encoded: base64) { value = decoded }
            else { throw HandFailure.invalidInput }
            let response = try optionalBool("with_response", in: fields)
            try await bridge.write(serviceUUID: service, characteristicUUID: characteristic, value: value, withResponse: response)
            return .object(["written_bytes": .number(Double(value.count))])
        case "bluetooth_subscribe":
            try exact(fields, allowed: ["service_uuid", "characteristic_uuid", "enabled"])
            let (service, characteristic) = try uuids(fields)
            guard let enabled = try optionalBool("enabled", in: fields) else { throw HandFailure.invalidInput }
            try await bridge.setNotifications(enabled, serviceUUID: service, characteristicUUID: characteristic)
            return .object(["notifying": .bool(enabled)])
        case "bluetooth_collect":
            try exact(fields, allowed: ["duration_seconds", "service_uuid", "characteristic_uuid"])
            guard let duration = try optionalNumber("duration_seconds", in: fields, range: 0.1...120) else { throw HandFailure.invalidInput }
            let service = try optionalString("service_uuid", in: fields, maximum: 64)
            let characteristic = try optionalString("characteristic_uuid", in: fields, maximum: 64)
            let events = try await bridge.collect(duration: duration, serviceUUID: service, characteristicUUID: characteristic)
            return .object(["notifications": .array(events.map { event in .object([
                "service_uuid": .string(event.serviceUUID), "characteristic_uuid": .string(event.characteristicUUID),
                "timestamp": .string(ISO8601DateFormatter().string(from: event.timestamp)), "value": valueJSON(event.value)
            ]) }), "count": .number(Double(events.count))])
        default: throw HandFailure.invalidInput
        }
    }

    private static func deviceJSON(_ device: BluetoothLEDevice) -> JSON { .object([
        "device_id": .string(device.id), "name": .string(device.name), "rssi": device.rssi.map { .number(Double($0)) } ?? .null,
        "connected": .bool(device.connected), "advertised_services": .array(device.advertisedServices.map(JSON.string))
    ]) }
    private static func valueJSON(_ value: Data) -> JSON {
        var fields: [String: JSON] = ["size": .number(Double(value.count)), "base64": .string(value.base64EncodedString()),
                                           "hex": .string(value.map { String(format: "%02x", $0) }.joined())]
        if let utf8 = String(data: value, encoding: .utf8) { fields["utf8"] = .string(utf8) }
        return .object(fields)
    }
    private static func uuids(_ fields: [String: JSON]) throws -> (String, String) {
        (try requiredString("service_uuid", in: fields, maximum: 64), try requiredString("characteristic_uuid", in: fields, maximum: 64))
    }
    private static func hexData(_ value: String) throws -> Data {
        guard value.count.isMultiple(of: 2), value.range(of: #"^[0-9A-Fa-f]*$"#, options: .regularExpression) != nil else { throw HandFailure.invalidInput }
        var data = Data(), index = value.startIndex
        while index < value.endIndex {
            let next = value.index(index, offsetBy: 2)
            guard let byte = UInt8(value[index..<next], radix: 16) else { throw HandFailure.invalidInput }
            data.append(byte); index = next
        }
        return data
    }
    private static func exact(_ fields: [String: JSON], allowed: Set<String>) throws {
        guard Set(fields.keys).isSubset(of: allowed) else { throw HandFailure.invalidInput }
    }
    private static func requiredString(_ key: String, in fields: [String: JSON], maximum: Int) throws -> String {
        guard let value = try optionalString(key, in: fields, maximum: maximum), !value.isEmpty else { throw HandFailure.invalidInput }
        return value
    }
    private static func optionalString(_ key: String, in fields: [String: JSON], maximum: Int? = nil) throws -> String? {
        guard let value = fields[key] else { return nil }
        guard case let .string(text) = value, maximum.map({ text.utf8.count <= $0 }) ?? true else { throw HandFailure.invalidInput }
        return text
    }
    private static func optionalStrings(_ key: String, in fields: [String: JSON]) throws -> [String] {
        guard let value = fields[key] else { return [] }
        guard case let .array(values) = value, values.count <= 32 else { throw HandFailure.invalidInput }
        return try values.map { value in
            guard case let .string(text) = value, !text.isEmpty, text.utf8.count <= 64 else { throw HandFailure.invalidInput }
            return text
        }
    }
    private static func optionalBool(_ key: String, in fields: [String: JSON]) throws -> Bool? {
        guard let value = fields[key] else { return nil }; guard case let .bool(flag) = value else { throw HandFailure.invalidInput }; return flag
    }
    private static func optionalNumber(_ key: String, in fields: [String: JSON], range: ClosedRange<Double>) throws -> Double? {
        guard let value = fields[key] else { return nil }
        guard case let .number(number) = value, number.isFinite, range.contains(number) else { throw HandFailure.invalidInput }
        return number
    }
}
