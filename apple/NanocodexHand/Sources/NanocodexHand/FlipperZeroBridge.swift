import CoreBluetooth
import Foundation

struct FlipperZeroDevice: Equatable {
    let id: String
    let name: String
    let color: String
    let rssi: Int?
    let connected: Bool
}

struct FlipperZeroInfo {
    let device: FlipperZeroDevice
    let rpc: [(String, String)]
    let bluetooth: [String: String]
    let batteryPercent: Int?
}

enum FlipperZeroBridgeError: Error, LocalizedError {
    case bluetoothUnavailable(String)
    case noDevice
    case multipleDevices
    case invalidDevice
    case connectionFailed(String)
    case connectionTimedOut
    case notConnected
    case busy
    case rpcTimedOut
    case rpcStatus(UInt64)
    case responseTooLarge(Int)
    case invalidScreen

    var errorDescription: String? {
        switch self {
        case let .bluetoothUnavailable(reason): return "Bluetooth is unavailable: \(reason)."
        case .noDevice: return "No Flipper Zero was found. Turn on Bluetooth on the Flipper and keep it nearby."
        case .multipleDevices: return "Multiple Flipper Zero devices were found; pass the device_id returned by flipper_devices."
        case .invalidDevice: return "The Flipper device_id is invalid or no longer available."
        case let .connectionFailed(reason): return "Could not connect to Flipper Zero: \(reason). Disconnect it from the official mobile app and try again."
        case .connectionTimedOut: return "Pairing with Flipper Zero timed out. Accept the code shown by iOS and the Flipper, then try again."
        case .notConnected: return "Flipper Zero disconnected during the operation."
        case .busy: return "Flipper Zero is already handling another RPC operation."
        case .rpcTimedOut: return "Flipper Zero did not answer the RPC request in time."
        case let .rpcStatus(status): return "Flipper Zero rejected the RPC request (\(Self.statusName(status)))."
        case let .responseTooLarge(limit): return "Flipper Zero returned more than the requested \(limit) bytes."
        case .invalidScreen: return "Flipper Zero returned an invalid 128×64 screen frame."
        }
    }

    private static func statusName(_ status: UInt64) -> String {
        switch status {
        case 1: return "error"
        case 2: return "decode error"
        case 3: return "not implemented"
        case 4: return "device busy"
        case 5: return "storage not ready"
        case 6: return "file exists"
        case 7: return "file not found"
        case 8: return "invalid storage parameter"
        case 9: return "storage access denied"
        case 10: return "invalid file name"
        case 11: return "storage error"
        case 12: return "storage operation not implemented"
        case 13: return "file already open"
        case 14: return "continuous command interrupted"
        case 15: return "invalid parameters"
        case 16: return "app could not start"
        case 17: return "another app has the device locked"
        case 18: return "directory is not empty"
        case 19: return "virtual display already started"
        case 20: return "virtual display not started"
        case 21: return "app is not running"
        case 22: return "app command failed"
        default: return "status \(status)"
        }
    }
}

/// Bluetooth LE companion for the Flipper Zero RPC serial service. The bridge
/// is lazy: constructing it never scans or asks for Bluetooth permission. The
/// first Flipper tool call starts CoreBluetooth and iOS/macOS owns pairing UI.
@MainActor
public final class FlipperZeroBridge: NSObject {
    public static let shared = FlipperZeroBridge()

    private struct Discovery {
        let peripheral: CBPeripheral
        var device: FlipperZeroDevice
    }

    private final class PendingRPC {
        let commandID: UInt64
        let operation: FlipperRPCOperation
        let maximumResponseBytes: Int?
        var frames: [FlipperRPCFrame] = []
        var observedResponseBytes = 0
        var overflow = false
        var result: Result<FlipperRPCResult, Error>?

        init(commandID: UInt64, operation: FlipperRPCOperation, maximumResponseBytes: Int?) {
            self.commandID = commandID
            self.operation = operation
            self.maximumResponseBytes = maximumResponseBytes
        }
    }

    private let advertisedServices = [CBUUID(string: "3080"), CBUUID(string: "3081"), CBUUID(string: "3082"), CBUUID(string: "3083")]
    private let serialService = CBUUID(string: "8FE5B3D5-2E7F-4A98-2A48-7ACC60FE0000")
    private let serialReadID = CBUUID(string: "19ED82AE-ED21-4C9D-4145-228E61FE0000")
    private let serialWriteID = CBUUID(string: "19ED82AE-ED21-4C9D-4145-228E62FE0000")
    private let flowControlID = CBUUID(string: "19ED82AE-ED21-4C9D-4145-228E63FE0000")
    private let restartSessionID = CBUUID(string: "19ED82AE-ED21-4C9D-4145-228E64FE0000")
    private let lastDeviceKey = "nanocodex.flipper-zero.last-device"

    private var manager: CBCentralManager?
    private var discoveries: [UUID: Discovery] = [:]
    private var peripheral: CBPeripheral?
    private var serialRead: CBCharacteristic?
    private var serialWrite: CBCharacteristic?
    private var flowControl: CBCharacteristic?
    private var restartSession: CBCharacteristic?
    private var bluetoothValues: [String: String] = [:]
    private var batteryPercent: Int?
    private var flowKnown = false
    private var freeSpace = 0
    private var ready = false
    private var connectionError: Error?
    private var outbound = Data()
    private var inbound = Data()
    private var nextCommandID: UInt64 = 1
    private var pending: PendingRPC?
    private var lastScreen: FlipperRPCScreen?
    private var screenRevision = 0

    public override init() {
        super.init()
    }

    var state: String {
        guard let manager else { return "idle" }
        if ready { return "connected" }
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

    func devices(scanFor seconds: Double = 3) async throws -> [FlipperZeroDevice] {
        let manager = try await poweredManager()
        discoveries = discoveries.filter { $0.value.peripheral.state == .connected }
        for connected in manager.retrieveConnectedPeripherals(withServices: advertisedServices) {
            remember(connected, advertisement: [:], rssi: nil)
        }
        manager.scanForPeripherals(withServices: advertisedServices, options: [CBCentralManagerScanOptionAllowDuplicatesKey: false])
        defer { manager.stopScan() }
        try await Task.sleep(for: .milliseconds(Int64(max(0.5, min(seconds, 10)) * 1_000)))
        try Task.checkCancellation()
        return discoveries.values.map(\.device).sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
    }

    @discardableResult
    func connect(deviceID: String?) async throws -> FlipperZeroDevice {
        let manager = try await poweredManager()
        if ready, let peripheral, deviceID == nil || peripheral.identifier.uuidString.caseInsensitiveCompare(deviceID!) == .orderedSame {
            return device(for: peripheral, rssi: discoveries[peripheral.identifier]?.device.rssi)
        }

        let target: CBPeripheral
        if let deviceID {
            guard let id = UUID(uuidString: deviceID) else { throw FlipperZeroBridgeError.invalidDevice }
            if let known = discoveries[id]?.peripheral { target = known }
            else if let restored = manager.retrievePeripherals(withIdentifiers: [id]).first { target = restored }
            else { throw FlipperZeroBridgeError.invalidDevice }
        } else if let saved = UserDefaults.standard.string(forKey: lastDeviceKey),
                  let id = UUID(uuidString: saved),
                  let restored = manager.retrievePeripherals(withIdentifiers: [id]).first {
            target = restored
        } else {
            let found = try await devices()
            guard !found.isEmpty else { throw FlipperZeroBridgeError.noDevice }
            guard found.count == 1, let known = UUID(uuidString: found[0].id).flatMap({ discoveries[$0]?.peripheral }) else {
                throw FlipperZeroBridgeError.multipleDevices
            }
            target = known
        }

        if let current = peripheral, current.identifier != target.identifier {
            manager.cancelPeripheralConnection(current)
        }
        resetLink(keepingPeripheral: false)
        peripheral = target
        target.delegate = self
        manager.connect(target)

        let deadline = Date().addingTimeInterval(30)
        while !ready {
            try Task.checkCancellation()
            if let connectionError { throw connectionError }
            guard Date() < deadline else {
                manager.cancelPeripheralConnection(target)
                throw FlipperZeroBridgeError.connectionTimedOut
            }
            try await Task.sleep(for: .milliseconds(50))
        }
        UserDefaults.standard.set(target.identifier.uuidString, forKey: lastDeviceKey)
        remember(target, advertisement: [:], rssi: discoveries[target.identifier]?.device.rssi)
        return device(for: target, rssi: discoveries[target.identifier]?.device.rssi)
    }

    func disconnect(forget: Bool) {
        if let peripheral { manager?.cancelPeripheralConnection(peripheral) }
        resetLink(keepingPeripheral: false)
        if forget { UserDefaults.standard.removeObject(forKey: lastDeviceKey) }
    }

    func info() async throws -> FlipperZeroInfo {
        let device = try await connect(deviceID: nil)
        guard case let .deviceInfo(values) = try await request(.deviceInfo) else { throw FlipperRPCWireError.malformed }
        return .init(device: device, rpc: values, bluetooth: bluetoothValues, batteryPercent: batteryPercent)
    }

    func list(path: String) async throws -> [FlipperRPCFile] {
        _ = try await connect(deviceID: nil)
        guard case let .files(files) = try await request(.list(path: path)) else { throw FlipperRPCWireError.malformed }
        return files
    }

    func read(path: String, maximumBytes: Int?) async throws -> Data {
        _ = try await connect(deviceID: nil)
        guard case let .data(data) = try await request(.read(path: path), maximumResponseBytes: maximumBytes) else {
            throw FlipperRPCWireError.malformed
        }
        return data
    }

    func write(path: String, data: Data) async throws {
        _ = try await connect(deviceID: nil)
        _ = try await request(.write(path: path, data: data), timeout: max(30, Double(data.count) / 2_048 + 15))
    }

    func startApp(name: String, args: String) async throws {
        _ = try await connect(deviceID: nil)
        _ = try await request(.startApp(name: name, args: args))
    }

    func exitApp() async throws {
        _ = try await connect(deviceID: nil)
        _ = try await request(.exitApp)
    }

    func press(key: UInt64, long: Bool) async throws {
        _ = try await connect(deviceID: nil)
        _ = try await request(.input(key: key, type: 0))
        _ = try await request(.input(key: key, type: long ? 3 : 2))
        _ = try await request(.input(key: key, type: 1))
    }

    func screen() async throws -> FlipperRPCScreen {
        _ = try await connect(deviceID: nil)
        let revision = screenRevision
        _ = try await request(.screenStream(start: true))
        do {
            let deadline = Date().addingTimeInterval(4)
            while screenRevision == revision {
                try Task.checkCancellation()
                guard Date() < deadline else { throw FlipperZeroBridgeError.rpcTimedOut }
                try await Task.sleep(for: .milliseconds(30))
            }
            _ = try await request(.screenStream(start: false))
        } catch {
            _ = try? await request(.screenStream(start: false))
            throw error
        }
        guard let lastScreen, lastScreen.bytes.count >= 1_024 else { throw FlipperZeroBridgeError.invalidScreen }
        return lastScreen
    }

    private func poweredManager() async throws -> CBCentralManager {
        let manager: CBCentralManager
        if let current = self.manager { manager = current }
        else {
            let created = CBCentralManager(delegate: self, queue: nil, options: [CBCentralManagerOptionShowPowerAlertKey: true])
            self.manager = created
            manager = created
        }
        let deadline = Date().addingTimeInterval(8)
        while true {
            try Task.checkCancellation()
            switch manager.state {
            case .poweredOn: return manager
            case .poweredOff: throw FlipperZeroBridgeError.bluetoothUnavailable("Bluetooth is turned off")
            case .unauthorized: throw FlipperZeroBridgeError.bluetoothUnavailable("Nanocodex does not have Bluetooth permission")
            case .unsupported: throw FlipperZeroBridgeError.bluetoothUnavailable("this device does not support Bluetooth LE")
            case .unknown, .resetting:
                guard Date() < deadline else { throw FlipperZeroBridgeError.bluetoothUnavailable("CoreBluetooth did not become ready") }
                try await Task.sleep(for: .milliseconds(50))
            @unknown default:
                throw FlipperZeroBridgeError.bluetoothUnavailable("unknown CoreBluetooth state")
            }
        }
    }

    private func request(
        _ operation: FlipperRPCOperation,
        maximumResponseBytes: Int? = nil,
        timeout: TimeInterval = 30
    ) async throws -> FlipperRPCResult {
        guard ready, peripheral?.state == .connected else { throw FlipperZeroBridgeError.notConnected }
        guard pending == nil else { throw FlipperZeroBridgeError.busy }
        let commandID = nextCommandID
        nextCommandID = commandID == UInt64(UInt32.max) ? 1 : commandID + 1
        let call = PendingRPC(commandID: commandID, operation: operation, maximumResponseBytes: maximumResponseBytes)
        pending = call
        for frame in FlipperRPCWire.requestFrames(commandID: commandID, operation: operation) { outbound.append(frame) }
        flush()

        let deadline = Date().addingTimeInterval(timeout)
        do {
            while call.result == nil {
                try Task.checkCancellation()
                guard ready, peripheral?.state == .connected else { throw FlipperZeroBridgeError.notConnected }
                guard Date() < deadline else { throw FlipperZeroBridgeError.rpcTimedOut }
                try await Task.sleep(for: .milliseconds(20))
            }
            pending = nil
            return try call.result!.get()
        } catch {
            if pending === call {
                pending = nil
                outbound.removeAll(keepingCapacity: true)
                restartRPCSession()
            }
            throw error
        }
    }

    private func flush() {
        guard ready, freeSpace > 0, let peripheral, let serialWrite else { return }
        let mtu = max(1, peripheral.maximumWriteValueLength(for: .withoutResponse))
        while !outbound.isEmpty, freeSpace > 0 {
            let count = min(outbound.count, min(freeSpace, mtu))
            let chunk = outbound.prefix(count)
            outbound.removeFirst(count)
            freeSpace -= count
            peripheral.writeValue(Data(chunk), for: serialWrite, type: .withResponse)
        }
    }

    private func receive(_ data: Data) {
        inbound.append(data)
        do {
            for message in try FlipperRPCWire.takeMessages(from: &inbound) {
                let frame = try FlipperRPCWire.decodeMain(message)
                if frame.commandID == 0 {
                    if let screen = try FlipperRPCWire.screen(from: frame) {
                        lastScreen = screen
                        screenRevision += 1
                    }
                    continue
                }
                guard let pending, pending.commandID == frame.commandID else { continue }
                guard frame.status == 0 else {
                    pending.result = .failure(FlipperZeroBridgeError.rpcStatus(frame.status))
                    continue
                }
                if case .read = pending.operation {
                    pending.observedResponseBytes += try FlipperRPCWire.readDataByteCount(in: frame)
                }
                if let maximum = pending.maximumResponseBytes, pending.observedResponseBytes > maximum {
                    pending.overflow = true
                } else if !pending.overflow {
                    pending.frames.append(frame)
                }
                if !frame.hasNext {
                    if let maximum = pending.maximumResponseBytes, pending.overflow {
                        pending.result = .failure(FlipperZeroBridgeError.responseTooLarge(maximum))
                    } else {
                        pending.result = Result { try FlipperRPCWire.result(from: pending.frames, for: pending.operation) }
                    }
                }
            }
        } catch {
            pending?.result = .failure(error)
        }
    }

    private func remember(_ peripheral: CBPeripheral, advertisement: [String: Any], rssi: Int?) {
        let service = (advertisement[CBAdvertisementDataServiceUUIDsKey] as? [CBUUID])?.first
        let color: String
        switch service?.uuidString.uppercased() {
        case "3081": color = "black"
        case "3082": color = "white"
        case "3083": color = "transparent"
        default: color = discoveries[peripheral.identifier]?.device.color ?? "unknown"
        }
        let advertisedName = advertisement[CBAdvertisementDataLocalNameKey] as? String
        let rawName = advertisedName ?? peripheral.name ?? discoveries[peripheral.identifier]?.device.name ?? "Flipper Zero"
        let name = rawName.hasPrefix("Flipper ") ? String(rawName.dropFirst("Flipper ".count)) : rawName
        discoveries[peripheral.identifier] = .init(
            peripheral: peripheral,
            device: .init(id: peripheral.identifier.uuidString, name: name, color: color, rssi: rssi, connected: peripheral.state == .connected)
        )
    }

    private func device(for peripheral: CBPeripheral, rssi: Int?) -> FlipperZeroDevice {
        remember(peripheral, advertisement: [:], rssi: rssi)
        return discoveries[peripheral.identifier]!.device
    }

    private func updateReady() {
        ready = peripheral?.state == .connected && serialWrite != nil && serialRead?.isNotifying == true
            && flowControl?.isNotifying == true && flowKnown
        if ready { flush() }
    }

    private func failConnection(_ error: Error) {
        connectionError = error
        pending?.result = .failure(error)
        ready = false
    }

    private func resetLink(keepingPeripheral: Bool) {
        ready = false
        connectionError = nil
        serialRead = nil
        serialWrite = nil
        flowControl = nil
        restartSession = nil
        flowKnown = false
        freeSpace = 0
        bluetoothValues = [:]
        batteryPercent = nil
        outbound.removeAll(keepingCapacity: true)
        inbound.removeAll(keepingCapacity: true)
        pending?.result = .failure(FlipperZeroBridgeError.notConnected)
        pending = nil
        if !keepingPeripheral { peripheral = nil }
    }

    private func restartRPCSession() {
        guard let peripheral, peripheral.state == .connected, let restartSession else { return }
        peripheral.writeValue(Data([0]), for: restartSession, type: .withResponse)
        inbound.removeAll(keepingCapacity: true)
    }
}

extension FlipperZeroBridge: @preconcurrency CBCentralManagerDelegate {
    public func centralManagerDidUpdateState(_ central: CBCentralManager) {
        if central.state != .poweredOn, peripheral != nil {
            failConnection(FlipperZeroBridgeError.bluetoothUnavailable("CoreBluetooth state changed"))
        }
    }

    public func centralManager(
        _ central: CBCentralManager,
        didDiscover peripheral: CBPeripheral,
        advertisementData: [String: Any],
        rssi RSSI: NSNumber
    ) {
        remember(peripheral, advertisement: advertisementData, rssi: RSSI.intValue)
    }

    public func centralManager(_ central: CBCentralManager, didConnect peripheral: CBPeripheral) {
        guard peripheral.identifier == self.peripheral?.identifier else { return }
        peripheral.delegate = self
        peripheral.discoverServices([serialService, CBUUID(string: "180A"), CBUUID(string: "180F")])
    }

    public func centralManager(_ central: CBCentralManager, didFailToConnect peripheral: CBPeripheral, error: Error?) {
        guard peripheral.identifier == self.peripheral?.identifier else { return }
        failConnection(FlipperZeroBridgeError.connectionFailed(error?.localizedDescription ?? "unknown error"))
    }

    public func centralManager(_ central: CBCentralManager, didDisconnectPeripheral peripheral: CBPeripheral, error: Error?) {
        guard peripheral.identifier == self.peripheral?.identifier else { return }
        let failure = FlipperZeroBridgeError.connectionFailed(error?.localizedDescription ?? "device disconnected")
        resetLink(keepingPeripheral: false)
        connectionError = failure
        remember(peripheral, advertisement: [:], rssi: discoveries[peripheral.identifier]?.device.rssi)
    }
}

extension FlipperZeroBridge: @preconcurrency CBPeripheralDelegate {
    public func peripheral(_ peripheral: CBPeripheral, didDiscoverServices error: Error?) {
        guard error == nil else { return failConnection(FlipperZeroBridgeError.connectionFailed(error!.localizedDescription)) }
        peripheral.services?.forEach { peripheral.discoverCharacteristics(nil, for: $0) }
    }

    public func peripheral(_ peripheral: CBPeripheral, didDiscoverCharacteristicsFor service: CBService, error: Error?) {
        guard error == nil else { return failConnection(FlipperZeroBridgeError.connectionFailed(error!.localizedDescription)) }
        for characteristic in service.characteristics ?? [] {
            switch characteristic.uuid {
            case serialReadID:
                serialRead = characteristic
                peripheral.setNotifyValue(true, for: characteristic)
            case serialWriteID:
                serialWrite = characteristic
            case flowControlID:
                flowControl = characteristic
                peripheral.setNotifyValue(true, for: characteristic)
                peripheral.readValue(for: characteristic)
            case restartSessionID:
                restartSession = characteristic
            case CBUUID(string: "2A19"):
                peripheral.setNotifyValue(true, for: characteristic)
                peripheral.readValue(for: characteristic)
            default:
                if service.uuid == CBUUID(string: "180A") { peripheral.readValue(for: characteristic) }
            }
        }
        updateReady()
    }

    public func peripheral(_ peripheral: CBPeripheral, didUpdateNotificationStateFor characteristic: CBCharacteristic, error: Error?) {
        guard error == nil else { return failConnection(FlipperZeroBridgeError.connectionFailed(error!.localizedDescription)) }
        updateReady()
    }

    public func peripheral(_ peripheral: CBPeripheral, didUpdateValueFor characteristic: CBCharacteristic, error: Error?) {
        guard error == nil else { return failConnection(FlipperZeroBridgeError.connectionFailed(error!.localizedDescription)) }
        guard let data = characteristic.value else { return }
        switch characteristic.uuid {
        case serialReadID:
            receive(data)
        case flowControlID:
            guard data.count == 4 else { return failConnection(FlipperRPCWireError.malformed) }
            freeSpace = data.reduce(0) { ($0 << 8) | Int($1) }
            flowKnown = true
            updateReady()
            flush()
        case CBUUID(string: "2A19"):
            batteryPercent = data.first.map(Int.init)
        default:
            let labels: [String: String] = [
                "2A29": "manufacturer", "2A25": "serial_number", "2A26": "firmware_revision",
                "2A28": "software_revision", "03F6666D-AE5E-47C8-8E1A-5D873EB5A933": "protobuf_revision"
            ]
            if let label = labels[characteristic.uuid.uuidString.uppercased()] {
                bluetoothValues[label] = String(data: data, encoding: .utf8) ?? data.base64EncodedString()
            }
        }
    }

    public func peripheral(_ peripheral: CBPeripheral, didWriteValueFor characteristic: CBCharacteristic, error: Error?) {
        if let error { pending?.result = .failure(FlipperZeroBridgeError.connectionFailed(error.localizedDescription)) }
    }
}
