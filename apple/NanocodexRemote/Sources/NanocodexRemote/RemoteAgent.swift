import Foundation

struct RemoteAgentInput: Codable, Sendable {
    let action: String
    var x: Double?, y: Double?, endX: Double?, endY: Double?
    var button: Int?, text: String?, key: UInt16?, modifiers: [UInt16]?
    var deltaX: Double?, deltaY: Double?, durationMs: Int?

    func steps(generation: String) throws -> [(delay: Int, input: RemoteInput)] {
        var steps: [(Int, RemoteInput)] = []
        func add(_ kind: RemoteInput.Kind, delay: Int = 0, x: Double? = nil, y: Double? = nil,
                 button: Int? = nil, down: Bool? = nil, key: UInt16? = nil,
                 text: String? = nil, deltaX: Double? = nil, deltaY: Double? = nil) {
            steps.append((delay, RemoteInput(kind: kind, sequence: UInt64(steps.count + 1), generation: generation,
                x: x, y: y, button: button, down: down, key: key, text: text, deltaX: deltaX, deltaY: deltaY)))
        }
        switch action {
        case "observe", "release": break
        case "click":
            for down in [true, false] { add(.button, x: x, y: y, button: button ?? 0, down: down) }
        case "type": add(.text, text: text)
        case "key":
            let modifiers = modifiers ?? []
            guard modifiers.count <= 4, Set(modifiers).count == modifiers.count,
                  modifiers.allSatisfy({ (224...231).contains($0) }) else { throw RemoteError.invalidMessage }
            for modifier in modifiers { add(.key, down: true, key: modifier) }
            for down in [true, false] { add(.key, down: down, key: key) }
            for modifier in modifiers.reversed() { add(.key, down: false, key: modifier) }
        case "scroll": add(.scroll, x: x, y: y, deltaX: deltaX, deltaY: deltaY)
        case "drag":
            guard let x, let y, let endX, let endY, (50...1500).contains(durationMs ?? 300) else { throw RemoteError.invalidMessage }
            let count = max(2, (durationMs ?? 300) / 33), delay = (durationMs ?? 300) / count
            add(.button, x: x, y: y, button: 0, down: true)
            for index in 1...count {
                let fraction = Double(index) / Double(count)
                add(.move, delay: delay, x: x + (endX - x) * fraction, y: y + (endY - y) * fraction)
            }
            add(.button, x: endX, y: endY, button: 0, down: false)
        default: throw RemoteError.invalidMessage
        }
        for step in steps { try step.1.validate() }
        return steps
    }
}

#if os(macOS)
import CoreImage
import CoreVideo
import ImageIO

struct RemoteSnapshot: Sendable { let jpeg: Data; let width: Int; let height: Int }

/// Keeps one captured frame, independent of viewer count. JPEG encoding occurs
/// only for an agent observation, outside the live video capture callback.
final class RemoteSnapshotBuffer: @unchecked Sendable {
    private let lock = NSLock()
    private var frame: CVPixelBuffer?
    func update(_ frame: CVPixelBuffer) { lock.withLock { self.frame = frame } }
    func clear() { lock.withLock { frame = nil } }
    func snapshot() throws -> RemoteSnapshot {
        guard let frame = lock.withLock({ frame }) else { throw RemoteError.unavailable }
        var image = CIImage(cvPixelBuffer: frame)
        let scale = min(1, 1280 / max(image.extent.width, image.extent.height))
        image = image.transformed(by: CGAffineTransform(scaleX: scale, y: scale))
        let context = CIContext(), space = CGColorSpaceCreateDeviceRGB()
        guard let jpeg = context.jpegRepresentation(of: image, colorSpace: space,
            options: [CIImageRepresentationOption(rawValue: kCGImageDestinationLossyCompressionQuality as String): 0.65]),
              jpeg.count <= 500_000 else { throw RemoteError.unavailable }
        return RemoteSnapshot(jpeg: jpeg, width: Int(image.extent.width), height: Int(image.extent.height))
    }
}
#endif
