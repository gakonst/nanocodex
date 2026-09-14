import Foundation
import ImageIO

enum RemoteRelayData: Codable, Sendable {
    case control(RemoteControlMessage)
    case input(RemoteInput)
    init(from decoder: Decoder) throws {
        let value = try decoder.singleValueContainer()
        if let control = try? value.decode(RemoteControlMessage.self) { self = .control(control) }
        else { self = .input(try value.decode(RemoteInput.self)) }
    }
    func encode(to encoder: Encoder) throws {
        var value = encoder.singleValueContainer()
        switch self {
        case .control(let message): try value.encode(message)
        case .input(let message): try value.encode(message)
        }
    }
}

enum RemoteFrame {
    static func decode(_ message: RemoteMessage) throws -> CGImage {
        guard message.type == "frame", let jpeg = message.jpeg, jpeg.utf8.count <= 700_000,
              let width = message.width, let height = message.height,
              (1...1280).contains(width), (1...1280).contains(height),
              let bytes = Data(base64Encoded: jpeg), bytes.starts(with: [0xff, 0xd8, 0xff]),
              let source = CGImageSourceCreateWithData(bytes as CFData, [kCGImageSourceShouldCache: false] as CFDictionary),
              CGImageSourceGetCount(source) == 1,
              let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any],
              (properties[kCGImagePropertyPixelWidth] as? Int) == width,
              (properties[kCGImagePropertyPixelHeight] as? Int) == height,
              let image = CGImageSourceCreateImageAtIndex(source, 0, nil)
        else { throw RemoteError.invalidMessage }
        return image
    }
}
