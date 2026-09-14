import Foundation

/// The small, dependency-free slice of Flipper Zero's public protobuf RPC wire
/// protocol used by the native Hand. Keeping this codec local avoids coupling
/// the app to generated sources while preserving the protocol's length-delimited
/// framing and command correlation.
///
/// Protocol source: https://github.com/flipperdevices/flipperzero-protobuf
enum FlipperRPCOperation: Equatable {
    case deviceInfo
    case list(path: String)
    case read(path: String)
    case write(path: String, data: Data)
    case startApp(name: String, args: String)
    case exitApp
    case input(key: UInt64, type: UInt64)
    case screenStream(start: Bool)
}

struct FlipperRPCFile: Equatable {
    let name: String
    let isDirectory: Bool
    let size: UInt64
    let md5: String?
}

struct FlipperRPCScreen: Equatable {
    let bytes: Data
    let orientation: UInt64

    var braille: String {
        guard bytes.count >= 128 * 8 else { return "" }
        let dots = [[0, 1, 2, 6], [3, 4, 5, 7]]
        var lines: [String] = []
        for top in stride(from: 0, to: 64, by: 4) {
            var line = ""
            for left in stride(from: 0, to: 128, by: 2) {
                var mask = 0
                for column in 0..<2 {
                    for row in 0..<4 where pixel(x: left + column, y: top + row) {
                        mask |= 1 << dots[column][row]
                    }
                }
                line.unicodeScalars.append(UnicodeScalar(0x2800 + mask)!)
            }
            lines.append(line)
        }
        return lines.joined(separator: "\n")
    }

    private func pixel(x: Int, y: Int) -> Bool {
        let byte = bytes[(y / 8) * 128 + x]
        return (byte & (1 << UInt8(y & 7))) != 0
    }
}

enum FlipperRPCResult: Equatable {
    case ok
    case deviceInfo([(String, String)])
    case files([FlipperRPCFile])
    case data(Data)

    static func == (lhs: Self, rhs: Self) -> Bool {
        switch (lhs, rhs) {
        case (.ok, .ok): return true
        case let (.deviceInfo(a), .deviceInfo(b)):
            return a.elementsEqual(b) { $0.0 == $1.0 && $0.1 == $1.1 }
        case let (.files(a), .files(b)): return a == b
        case let (.data(a), .data(b)): return a == b
        default: return false
        }
    }
}

struct FlipperRPCFrame {
    let commandID: UInt64
    let status: UInt64
    let hasNext: Bool
    fileprivate let fields: [Int: [FlipperProtoValue]]

    fileprivate func messages(_ number: Int) -> [Data] {
        (fields[number] ?? []).compactMap {
            guard case let .bytes(value) = $0 else { return nil }
            return value
        }
    }
}

enum FlipperRPCWireError: Error, LocalizedError {
    case malformed
    case oversized
    case unsupportedWireType

    var errorDescription: String? {
        switch self {
        case .malformed: return "Flipper returned malformed RPC data."
        case .oversized: return "Flipper returned an oversized RPC frame."
        case .unsupportedWireType: return "Flipper returned an unsupported protobuf field."
        }
    }
}

enum FlipperRPCWire {
    static let maximumFrameBytes = 1_048_576
    static let writeChunkBytes = 512

    static func requestFrames(commandID: UInt64, operation: FlipperRPCOperation) -> [Data] {
        let messages: [(field: Int, payload: Data)]
        switch operation {
        case .deviceInfo:
            messages = [(32, Data())]
        case let .list(path):
            messages = [(7, message([bytesField(1, Data(path.utf8))]))]
        case let .read(path):
            messages = [(9, message([bytesField(1, Data(path.utf8))]))]
        case let .write(path, data):
            let chunks = data.isEmpty ? [Data()] : stride(from: 0, to: data.count, by: writeChunkBytes).map {
                data.subdata(in: $0..<min(data.count, $0 + writeChunkBytes))
            }
            messages = chunks.map { chunk in
                let file = message([bytesField(4, chunk)])
                return (11, message([bytesField(1, Data(path.utf8)), bytesField(2, file)]))
            }
        case let .startApp(name, args):
            messages = [(16, message([bytesField(1, Data(name.utf8)), bytesField(2, Data(args.utf8))]))]
        case .exitApp:
            messages = [(47, Data())]
        case let .input(key, type):
            messages = [(23, message([varintField(1, key), varintField(2, type)]))]
        case let .screenStream(start):
            messages = [(start ? 20 : 21, Data())]
        }

        return messages.enumerated().map { index, content in
            var main = message([
                varintField(1, commandID),
                index + 1 < messages.count ? varintField(3, 1) : Data(),
                bytesField(content.field, content.payload)
            ])
            main.insert(contentsOf: encodeVarint(UInt64(main.count)), at: 0)
            return main
        }
    }

    /// Removes every complete length-delimited message from a BLE byte stream.
    /// An incomplete prefix or payload remains buffered for the next notification.
    static func takeMessages(from buffer: inout Data) throws -> [Data] {
        var result: [Data] = []
        while !buffer.isEmpty {
            guard let (length, prefixBytes) = try lengthPrefix(in: buffer) else { break }
            guard length <= maximumFrameBytes else { throw FlipperRPCWireError.oversized }
            guard buffer.count >= prefixBytes + length else { break }
            result.append(Data(buffer.dropFirst(prefixBytes).prefix(length)))
            buffer.removeFirst(prefixBytes + length)
        }
        return result
    }

    static func decodeMain(_ data: Data) throws -> FlipperRPCFrame {
        let fields = try decodeFields(data)
        return .init(
            commandID: firstVarint(1, in: fields) ?? 0,
            status: firstVarint(2, in: fields) ?? 0,
            hasNext: (firstVarint(3, in: fields) ?? 0) != 0,
            fields: fields
        )
    }

    static func result(from frames: [FlipperRPCFrame], for operation: FlipperRPCOperation) throws -> FlipperRPCResult {
        switch operation {
        case .deviceInfo:
            let values = try frames.flatMap { frame in
                try frame.messages(33).map { payload -> (String, String) in
                    let fields = try decodeFields(payload)
                    return (try stringField(1, in: fields), try stringField(2, in: fields))
                }
            }
            return .deviceInfo(values)
        case .list:
            let files = try frames.flatMap { frame in
                try frame.messages(8).flatMap { payload -> [FlipperRPCFile] in
                    let fields = try decodeFields(payload)
                    return try messages(1, in: fields).map(decodeFile)
                }
            }
            return .files(files)
        case .read:
            var data = Data()
            for frame in frames {
                for response in frame.messages(10) {
                    let fields = try decodeFields(response)
                    for file in messages(1, in: fields) {
                        let fileFields = try decodeFields(file)
                        for chunk in messages(4, in: fileFields) { data.append(chunk) }
                    }
                }
            }
            return .data(data)
        case .write, .startApp, .exitApp, .input, .screenStream:
            return .ok
        }
    }

    static func screen(from frame: FlipperRPCFrame) throws -> FlipperRPCScreen? {
        guard let payload = frame.messages(22).last else { return nil }
        let fields = try decodeFields(payload)
        guard let data = messages(1, in: fields).last else { throw FlipperRPCWireError.malformed }
        return .init(bytes: data, orientation: firstVarint(2, in: fields) ?? 0)
    }

    static func readDataByteCount(in frame: FlipperRPCFrame) throws -> Int {
        try frame.messages(10).reduce(0) { responseTotal, response in
            let responseFields = try decodeFields(response)
            return try messages(1, in: responseFields).reduce(responseTotal) { fileTotal, file in
                let fileFields = try decodeFields(file)
                return messages(4, in: fileFields).reduce(fileTotal) { $0 + $1.count }
            }
        }
    }

    private static func decodeFile(_ data: Data) throws -> FlipperRPCFile {
        let fields = try decodeFields(data)
        return .init(
            name: try stringField(2, in: fields),
            isDirectory: (firstVarint(1, in: fields) ?? 0) == 1,
            size: firstVarint(3, in: fields) ?? 0,
            md5: try optionalStringField(5, in: fields)
        )
    }

    private static func lengthPrefix(in data: Data) throws -> (Int, Int)? {
        var value: UInt64 = 0
        var shift: UInt64 = 0
        for (index, byte) in data.prefix(10).enumerated() {
            let low = UInt64(byte & 0x7f)
            if shift == 63, low > 1 { throw FlipperRPCWireError.malformed }
            value |= low << shift
            if byte & 0x80 == 0 {
                guard value <= UInt64(Int.max) else { throw FlipperRPCWireError.oversized }
                return (Int(value), index + 1)
            }
            shift += 7
        }
        if data.count >= 10 { throw FlipperRPCWireError.malformed }
        return nil
    }

    static func decodeFields(_ data: Data) throws -> [Int: [FlipperProtoValue]] {
        var fields: [Int: [FlipperProtoValue]] = [:]
        var index = 0
        while index < data.count {
            let key = try readVarint(data, index: &index)
            let number = Int(key >> 3), wireType = Int(key & 7)
            guard number > 0 else { throw FlipperRPCWireError.malformed }
            let value: FlipperProtoValue
            switch wireType {
            case 0:
                value = .varint(try readVarint(data, index: &index))
            case 1:
                guard data.count - index >= 8 else { throw FlipperRPCWireError.malformed }
                index += 8
                continue
            case 2:
                let rawLength = try readVarint(data, index: &index)
                guard rawLength <= UInt64(Int.max) else { throw FlipperRPCWireError.oversized }
                let length = Int(rawLength)
                guard length <= maximumFrameBytes, data.count - index >= length else {
                    throw length > maximumFrameBytes ? FlipperRPCWireError.oversized : FlipperRPCWireError.malformed
                }
                value = .bytes(data.subdata(in: index..<(index + length)))
                index += length
            case 5:
                guard data.count - index >= 4 else { throw FlipperRPCWireError.malformed }
                index += 4
                continue
            default:
                throw FlipperRPCWireError.unsupportedWireType
            }
            fields[number, default: []].append(value)
        }
        return fields
    }

    private static func readVarint(_ data: Data, index: inout Int) throws -> UInt64 {
        var value: UInt64 = 0
        var shift: UInt64 = 0
        for _ in 0..<10 {
            guard index < data.count else { throw FlipperRPCWireError.malformed }
            let byte = data[index]
            index += 1
            let low = UInt64(byte & 0x7f)
            if shift == 63, low > 1 { throw FlipperRPCWireError.malformed }
            value |= low << shift
            if byte & 0x80 == 0 { return value }
            shift += 7
        }
        throw FlipperRPCWireError.malformed
    }

    static func varintField(_ number: Int, _ value: UInt64) -> Data {
        Data(encodeVarint(UInt64(number << 3)) + encodeVarint(value))
    }

    static func bytesField(_ number: Int, _ value: Data) -> Data {
        var field = Data(encodeVarint(UInt64((number << 3) | 2)))
        field.append(contentsOf: encodeVarint(UInt64(value.count)))
        field.append(value)
        return field
    }

    static func message(_ fields: [Data]) -> Data {
        fields.reduce(into: Data()) { $0.append($1) }
    }

    static func encodeVarint(_ input: UInt64) -> [UInt8] {
        var value = input
        var bytes: [UInt8] = []
        repeat {
            var byte = UInt8(value & 0x7f)
            value >>= 7
            if value != 0 { byte |= 0x80 }
            bytes.append(byte)
        } while value != 0
        return bytes
    }

    private static func firstVarint(_ number: Int, in fields: [Int: [FlipperProtoValue]]) -> UInt64? {
        (fields[number] ?? []).compactMap {
            guard case let .varint(value) = $0 else { return nil }
            return value
        }.first
    }

    private static func messages(_ number: Int, in fields: [Int: [FlipperProtoValue]]) -> [Data] {
        (fields[number] ?? []).compactMap {
            guard case let .bytes(value) = $0 else { return nil }
            return value
        }
    }

    private static func stringField(_ number: Int, in fields: [Int: [FlipperProtoValue]]) throws -> String {
        guard let value = try optionalStringField(number, in: fields) else { throw FlipperRPCWireError.malformed }
        return value
    }

    private static func optionalStringField(_ number: Int, in fields: [Int: [FlipperProtoValue]]) throws -> String? {
        guard let data = messages(number, in: fields).first else { return nil }
        guard let value = String(data: data, encoding: .utf8) else { throw FlipperRPCWireError.malformed }
        return value
    }
}

enum FlipperProtoValue {
    case varint(UInt64)
    case bytes(Data)
}
