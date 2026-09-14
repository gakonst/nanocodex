import CoreBluetooth
import Foundation

struct BluetoothLEDevice: Equatable {
    let id: String
    let name: String
    let rssi: Int?
    let connected: Bool
    let advertisedServices: [String]
}

struct BluetoothLECharacteristicInfo: Equatable {
    let serviceUUID: String
    let characteristicUUID: String
    let properties: [String]
    let notifying: Bool
}

struct BluetoothLENotification: Equatable {
    let serviceUUID: String
    let characteristicUUID: String
    let timestamp: Date
    let value: Data
}

enum BluetoothLEBridgeError: Error, LocalizedError {
    case unavailable(String), noDevice, multipleDevices, invalidDevice, connectionFailed(String)
    case connectionTimedOut, notConnected, discoveryTimedOut, characteristicNotFound(String, String)
    case unsupportedOperation(String), operationInProgress, operationTimedOut

    var errorDescription: String? {
        switch self {
        case let .unavailable(reason): return "Bluetooth is unavailable: \(reason)."
        case .noDevice: return "No matching Bluetooth LE device was found. Keep it nearby, powered on, and advertising."
        case .multipleDevices: return "Multiple Bluetooth LE devices matched; pass a device_id returned by bluetooth_devices."
        case .invalidDevice: return "The Bluetooth device_id is invalid or is no longer available. Scan again."
        case let .connectionFailed(reason): return "Could not connect to the Bluetooth LE device: \(reason)."
        case .connectionTimedOut: return "Connecting to the Bluetooth LE device timed out."
        case .notConnected: return "The Bluetooth LE device is not connected."
        case .discoveryTimedOut: return "Bluetooth GATT service discovery timed out."
        case let .characteristicNotFound(service, characteristic):
            return "Bluetooth characteristic \(characteristic) was not found in service \(service)."
        case let .unsupportedOperation(operation): return "The Bluetooth characteristic does not support \(operation)."
        case .operationInProgress: return "Another Bluetooth GATT operation is already in progress."
        case .operationTimedOut: return "The Bluetooth GATT operation timed out."
        }
    }
}

/// A deliberately protocol-neutral CoreBluetooth central. Typed device profiles
/// can build on this transport; the raw tools expose the same GATT boundary.
/// Constructing the bridge does not scan or request Bluetooth permission.
@MainActor
public final class BluetoothLEBridge: NSObject {
    public static let shared = BluetoothLEBridge()

    private struct Discovery {
        let peripheral: CBPeripheral
        var device: BluetoothLEDevice
    }
    private struct CharacteristicKey: Hashable {
        let service: String
        let characteristic: String
    }
    private final class PendingValue {
        let key: CharacteristicKey
        var result: Result<Data, Error>?
        init(key: CharacteristicKey) { self.key = key }
    }
    private final class PendingWrite {
        let key: CharacteristicKey
        var result: Result<Void, Error>?
        init(key: CharacteristicKey) { self.key = key }
    }
    private final class PendingNotify {
        let key: CharacteristicKey
        let enabled: Bool
        var result: Result<Void, Error>?
        init(key: CharacteristicKey, enabled: Bool) { self.key = key; self.enabled = enabled }
    }

    private var manager: CBCentralManager?
    private var discoveries: [UUID: Discovery] = [:]
    private var peripheral: CBPeripheral?
    private var advertisedServices: [String] = []
    private var characteristics: [CharacteristicKey: CBCharacteristic] = [:]
    private var discoveryPending = Set<String>()
    private var discoveryComplete = false
    private var connectionError: Error?
    private var pendingValue: PendingValue?
    private var pendingWrite: PendingWrite?
    private var pendingNotify: PendingNotify?
    private var notifications: [BluetoothLENotification] = []

    public override init() { super.init() }

    var state: String {
        guard let manager else { return "idle" }
        if discoveryComplete, peripheral?.state == .connected { return "connected" }
        if manager.isScanning { return "scanning" }
        if peripheral?.state == .connecting { return "connecting" }
        switch manager.state {
        case .poweredOn: return "ready"
        case .poweredOff: return "bluetooth_off"
        case .unauthorized: return "unauthorized"
        case .unsupported: return "unsupported"
        case .resetting: return "resetting"
        case .unknown: return "starting"
        @unknown default: return "unknown"
        }
    }

    func devices(serviceUUIDs: [String] = [], namePrefix: String? = nil, scanFor seconds: Double = 3) async throws -> [BluetoothLEDevice] {
        let central = try await poweredManager()
        let services = try serviceUUIDs.map(Self.uuid)
        discoveries.removeAll(keepingCapacity: true)
        if !services.isEmpty {
            for connected in central.retrieveConnectedPeripherals(withServices: services) {
                remember(connected, advertisement: [:], rssi: nil)
            }
        }
        central.scanForPeripherals(withServices: services.isEmpty ? nil : services, options: [CBCentralManagerScanOptionAllowDuplicatesKey: false])
        defer { central.stopScan() }
        try await Task.sleep(for: .milliseconds(Int64(max(0.5, min(seconds, 10)) * 1_000)))
        try Task.checkCancellation()
        return discoveries.values.map(\.device).filter { device in
            guard let namePrefix, !namePrefix.isEmpty else { return true }
            return device.name.range(of: namePrefix, options: [.anchored, .caseInsensitive, .diacriticInsensitive]) != nil
        }.sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
    }

    @discardableResult
    func connect(deviceID: String, serviceUUIDs: [String]? = nil) async throws -> BluetoothLEDevice {
        let central = try await poweredManager()
        guard let id = UUID(uuidString: deviceID) else { throw BluetoothLEBridgeError.invalidDevice }
        let target = discoveries[id]?.peripheral ?? central.retrievePeripherals(withIdentifiers: [id]).first
        guard let target else { throw BluetoothLEBridgeError.invalidDevice }
        if discoveryComplete, target.identifier == peripheral?.identifier, target.state == .connected {
            return device(for: target)
        }
        if let current = peripheral, current.identifier != target.identifier { central.cancelPeripheralConnection(current) }
        resetLink(keepingPeripheral: false)
        peripheral = target
        target.delegate = self
        advertisedServices = try (serviceUUIDs ?? []).map(Self.uuid).map(\.uuidString)
        if target.state == .connected {
            let requested = advertisedServices.isEmpty ? nil : advertisedServices.map(CBUUID.init(string:))
            target.discoverServices(requested)
        } else {
            central.connect(target)
        }
        let deadline = Date().addingTimeInterval(30)
        while !discoveryComplete {
            try Task.checkCancellation()
            if let connectionError { throw connectionError }
            guard Date() < deadline else {
                central.cancelPeripheralConnection(target)
                throw target.state == .connected ? BluetoothLEBridgeError.discoveryTimedOut : BluetoothLEBridgeError.connectionTimedOut
            }
            try await Task.sleep(for: .milliseconds(50))
        }
        remember(target, advertisement: [:], rssi: discoveries[id]?.device.rssi)
        return device(for: target)
    }

    func disconnect() {
        if let peripheral { manager?.cancelPeripheralConnection(peripheral) }
        resetLink(keepingPeripheral: false)
    }

    func services() throws -> [BluetoothLECharacteristicInfo] {
        guard discoveryComplete, peripheral?.state == .connected else { throw BluetoothLEBridgeError.notConnected }
        return characteristics.map { key, characteristic in
            .init(serviceUUID: key.service, characteristicUUID: key.characteristic,
                  properties: Self.propertyNames(characteristic.properties), notifying: characteristic.isNotifying)
        }.sorted {
            ($0.serviceUUID, $0.characteristicUUID) < ($1.serviceUUID, $1.characteristicUUID)
        }
    }

    func read(serviceUUID: String, characteristicUUID: String) async throws -> Data {
        let (key, characteristic) = try resolve(serviceUUID, characteristicUUID)
        guard characteristic.properties.contains(.read) else { throw BluetoothLEBridgeError.unsupportedOperation("read") }
        guard pendingValue == nil else { throw BluetoothLEBridgeError.operationInProgress }
        let pending = PendingValue(key: key)
        pendingValue = pending
        peripheral!.readValue(for: characteristic)
        return try await wait(for: pending, timeout: 15) { $0.result }
    }

    func write(serviceUUID: String, characteristicUUID: String, value: Data, withResponse requestedResponse: Bool?) async throws {
        let (key, characteristic) = try resolve(serviceUUID, characteristicUUID)
        let canRespond = characteristic.properties.contains(.write)
        let canSkipResponse = characteristic.properties.contains(.writeWithoutResponse)
        let withResponse = requestedResponse ?? canRespond
        guard withResponse ? canRespond : canSkipResponse else {
            throw BluetoothLEBridgeError.unsupportedOperation(withResponse ? "write with response" : "write without response")
        }
        let type: CBCharacteristicWriteType = withResponse ? .withResponse : .withoutResponse
        let maximum = peripheral!.maximumWriteValueLength(for: type)
        guard value.count <= maximum else {
            throw BluetoothLEBridgeError.unsupportedOperation("a single write larger than the negotiated \(maximum)-byte value length")
        }
        if !withResponse {
            peripheral!.writeValue(value, for: characteristic, type: type)
            return
        }
        guard pendingWrite == nil else { throw BluetoothLEBridgeError.operationInProgress }
        let pending = PendingWrite(key: key)
        pendingWrite = pending
        peripheral!.writeValue(value, for: characteristic, type: type)
        _ = try await wait(for: pending, timeout: 15) { $0.result }
    }

    func setNotifications(_ enabled: Bool, serviceUUID: String, characteristicUUID: String) async throws {
        let (key, characteristic) = try resolve(serviceUUID, characteristicUUID)
        guard characteristic.properties.contains(.notify) || characteristic.properties.contains(.indicate) else {
            throw BluetoothLEBridgeError.unsupportedOperation("notifications")
        }
        if characteristic.isNotifying == enabled { return }
        guard pendingNotify == nil else { throw BluetoothLEBridgeError.operationInProgress }
        let pending = PendingNotify(key: key, enabled: enabled)
        pendingNotify = pending
        peripheral!.setNotifyValue(enabled, for: characteristic)
        _ = try await wait(for: pending, timeout: 15) { $0.result }
    }

    func collect(duration seconds: Double, serviceUUID: String? = nil, characteristicUUID: String? = nil) async throws -> [BluetoothLENotification] {
        guard discoveryComplete, peripheral?.state == .connected else { throw BluetoothLEBridgeError.notConnected }
        let serviceFilter = serviceUUID.map(Self.normalize)
        let characteristicFilter = characteristicUUID.map(Self.normalize)
        notifications.removeAll(keepingCapacity: true)
        try await Task.sleep(for: .milliseconds(Int64(seconds * 1_000)))
        try Task.checkCancellation()
        return notifications.filter { notification in
            (serviceFilter == nil || Self.normalize(notification.serviceUUID) == serviceFilter) &&
            (characteristicFilter == nil || Self.normalize(notification.characteristicUUID) == characteristicFilter)
        }
    }

    private func wait<T, P>(for pending: P, timeout: TimeInterval, result: (P) -> Result<T, Error>?) async throws -> T {
        let deadline = Date().addingTimeInterval(timeout)
        defer {
            if let value = pending as? PendingValue, pendingValue === value { pendingValue = nil }
            if let write = pending as? PendingWrite, pendingWrite === write { pendingWrite = nil }
            if let notify = pending as? PendingNotify, pendingNotify === notify { pendingNotify = nil }
        }
        while result(pending) == nil {
            try Task.checkCancellation()
            guard Date() < deadline else { throw BluetoothLEBridgeError.operationTimedOut }
            try await Task.sleep(for: .milliseconds(20))
        }
        return try result(pending)!.get()
    }

    private func resolve(_ serviceUUID: String, _ characteristicUUID: String) throws -> (CharacteristicKey, CBCharacteristic) {
        guard discoveryComplete, peripheral?.state == .connected else { throw BluetoothLEBridgeError.notConnected }
        let key = CharacteristicKey(service: Self.normalize(serviceUUID), characteristic: Self.normalize(characteristicUUID))
        guard let characteristic = characteristics[key] else {
            throw BluetoothLEBridgeError.characteristicNotFound(serviceUUID, characteristicUUID)
        }
        return (key, characteristic)
    }

    private func poweredManager() async throws -> CBCentralManager {
        let central: CBCentralManager
        if let manager { central = manager }
        else {
            central = CBCentralManager(delegate: self, queue: nil, options: [CBCentralManagerOptionShowPowerAlertKey: true])
            manager = central
        }
        let deadline = Date().addingTimeInterval(8)
        while true {
            try Task.checkCancellation()
            switch central.state {
            case .poweredOn: return central
            case .poweredOff: throw BluetoothLEBridgeError.unavailable("Bluetooth is turned off")
            case .unauthorized: throw BluetoothLEBridgeError.unavailable("Nanocodex does not have Bluetooth permission")
            case .unsupported: throw BluetoothLEBridgeError.unavailable("this device does not support Bluetooth LE")
            case .unknown, .resetting:
                guard Date() < deadline else { throw BluetoothLEBridgeError.unavailable("CoreBluetooth did not become ready") }
                try await Task.sleep(for: .milliseconds(50))
            @unknown default: throw BluetoothLEBridgeError.unavailable("unknown CoreBluetooth state")
            }
        }
    }

    private func remember(_ peripheral: CBPeripheral, advertisement: [String: Any], rssi: Int?) {
        let name = advertisement[CBAdvertisementDataLocalNameKey] as? String ?? peripheral.name
            ?? discoveries[peripheral.identifier]?.device.name ?? "Unnamed BLE device"
        let services = (advertisement[CBAdvertisementDataServiceUUIDsKey] as? [CBUUID])?.map(\.uuidString)
            ?? discoveries[peripheral.identifier]?.device.advertisedServices ?? []
        discoveries[peripheral.identifier] = .init(peripheral: peripheral, device: .init(
            id: peripheral.identifier.uuidString, name: name, rssi: rssi,
            connected: peripheral.state == .connected, advertisedServices: services.sorted()
        ))
    }

    private func device(for peripheral: CBPeripheral) -> BluetoothLEDevice {
        remember(peripheral, advertisement: [:], rssi: discoveries[peripheral.identifier]?.device.rssi)
        return discoveries[peripheral.identifier]!.device
    }

    private func fail(_ error: Error) {
        connectionError = error
        pendingValue?.result = .failure(error)
        pendingWrite?.result = .failure(error)
        pendingNotify?.result = .failure(error)
    }

    private func resetLink(keepingPeripheral: Bool) {
        discoveryComplete = false
        discoveryPending.removeAll()
        characteristics.removeAll(keepingCapacity: true)
        notifications.removeAll(keepingCapacity: true)
        connectionError = nil
        pendingValue = nil; pendingWrite = nil; pendingNotify = nil
        if !keepingPeripheral { peripheral = nil }
    }

    private static func uuid(_ value: String) throws -> CBUUID {
        let clean = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !clean.isEmpty, clean.utf8.count <= 64,
              clean.range(of: #"^[0-9A-Fa-f-]+$"#, options: .regularExpression) != nil else {
            throw BluetoothLEBridgeError.unsupportedOperation("the invalid UUID \(value)")
        }
        return CBUUID(string: clean)
    }

    private static func normalize(_ value: String) -> String { CBUUID(string: value).uuidString.uppercased() }

    private static func propertyNames(_ properties: CBCharacteristicProperties) -> [String] {
        var names: [String] = []
        if properties.contains(.broadcast) { names.append("broadcast") }
        if properties.contains(.read) { names.append("read") }
        if properties.contains(.writeWithoutResponse) { names.append("write_without_response") }
        if properties.contains(.write) { names.append("write") }
        if properties.contains(.notify) { names.append("notify") }
        if properties.contains(.indicate) { names.append("indicate") }
        if properties.contains(.authenticatedSignedWrites) { names.append("authenticated_signed_writes") }
        if properties.contains(.extendedProperties) { names.append("extended_properties") }
        return names
    }
}

extension BluetoothLEBridge: @preconcurrency CBCentralManagerDelegate {
    public func centralManagerDidUpdateState(_ central: CBCentralManager) {
        if central.state != .poweredOn, peripheral != nil { fail(BluetoothLEBridgeError.unavailable("CoreBluetooth state changed")) }
    }

    public func centralManager(_ central: CBCentralManager, didDiscover peripheral: CBPeripheral,
                               advertisementData: [String: Any], rssi RSSI: NSNumber) {
        remember(peripheral, advertisement: advertisementData, rssi: RSSI.intValue)
    }

    public func centralManager(_ central: CBCentralManager, didConnect peripheral: CBPeripheral) {
        guard peripheral.identifier == self.peripheral?.identifier else { return }
        peripheral.delegate = self
        let requested = advertisedServices.isEmpty ? nil : advertisedServices.map(CBUUID.init(string:))
        peripheral.discoverServices(requested)
    }

    public func centralManager(_ central: CBCentralManager, didFailToConnect peripheral: CBPeripheral, error: Error?) {
        guard peripheral.identifier == self.peripheral?.identifier else { return }
        fail(BluetoothLEBridgeError.connectionFailed(error?.localizedDescription ?? "unknown error"))
    }

    public func centralManager(_ central: CBCentralManager, didDisconnectPeripheral peripheral: CBPeripheral, error: Error?) {
        guard peripheral.identifier == self.peripheral?.identifier else { return }
        let failure = BluetoothLEBridgeError.connectionFailed(error?.localizedDescription ?? "device disconnected")
        resetLink(keepingPeripheral: false)
        connectionError = failure
        remember(peripheral, advertisement: [:], rssi: discoveries[peripheral.identifier]?.device.rssi)
    }
}

extension BluetoothLEBridge: @preconcurrency CBPeripheralDelegate {
    public func peripheral(_ peripheral: CBPeripheral, didDiscoverServices error: Error?) {
        guard error == nil else { return fail(BluetoothLEBridgeError.connectionFailed(error!.localizedDescription)) }
        let services = peripheral.services ?? []
        discoveryPending = Set(services.map { $0.uuid.uuidString.uppercased() })
        if services.isEmpty { discoveryComplete = true; return }
        services.forEach { peripheral.discoverCharacteristics(nil, for: $0) }
    }

    public func peripheral(_ peripheral: CBPeripheral, didDiscoverCharacteristicsFor service: CBService, error: Error?) {
        guard error == nil else { return fail(BluetoothLEBridgeError.connectionFailed(error!.localizedDescription)) }
        for characteristic in service.characteristics ?? [] {
            let key = CharacteristicKey(service: service.uuid.uuidString.uppercased(), characteristic: characteristic.uuid.uuidString.uppercased())
            characteristics[key] = characteristic
        }
        discoveryPending.remove(service.uuid.uuidString.uppercased())
        discoveryComplete = discoveryPending.isEmpty
    }

    public func peripheral(_ peripheral: CBPeripheral, didUpdateValueFor characteristic: CBCharacteristic, error: Error?) {
        guard let service = characteristic.service else { return }
        let key = CharacteristicKey(service: service.uuid.uuidString.uppercased(), characteristic: characteristic.uuid.uuidString.uppercased())
        if let pending = pendingValue, pending.key == key {
            pending.result = error.map { .failure($0) } ?? characteristic.value.map { .success($0) }
                ?? .failure(BluetoothLEBridgeError.connectionFailed("the characteristic returned no value"))
            return
        }
        guard error == nil, let value = characteristic.value else { return }
        notifications.append(.init(serviceUUID: key.service, characteristicUUID: key.characteristic, timestamp: Date(), value: value))
    }

    public func peripheral(_ peripheral: CBPeripheral, didWriteValueFor characteristic: CBCharacteristic, error: Error?) {
        guard let service = characteristic.service else { return }
        let key = CharacteristicKey(service: service.uuid.uuidString.uppercased(), characteristic: characteristic.uuid.uuidString.uppercased())
        guard let pending = pendingWrite, pending.key == key else { return }
        pending.result = error.map { .failure($0) } ?? .success(())
    }

    public func peripheral(_ peripheral: CBPeripheral, didUpdateNotificationStateFor characteristic: CBCharacteristic, error: Error?) {
        guard let service = characteristic.service else { return }
        let key = CharacteristicKey(service: service.uuid.uuidString.uppercased(), characteristic: characteristic.uuid.uuidString.uppercased())
        guard let pending = pendingNotify, pending.key == key else { return }
        if let error { pending.result = .failure(error) }
        else if characteristic.isNotifying == pending.enabled { pending.result = .success(()) }
        else { pending.result = .failure(BluetoothLEBridgeError.unsupportedOperation("the requested notification state")) }
    }
}
