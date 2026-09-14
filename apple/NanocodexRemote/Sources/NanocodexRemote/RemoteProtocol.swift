import Foundation

public enum RemoteError: LocalizedError, Equatable {
    case invalidMessage, unavailable, unauthorized, busy, closed, hostReplaced, screenPermission, inputPermission, geometryChanged, phoneBridge

    public var errorDescription: String? {
        switch self {
        case .invalidMessage: return "The remote session sent an invalid message."
        case .unavailable: return "This screen is unavailable."
        case .unauthorized: return "This remote session is no longer authorized."
        case .busy: return "Someone else is controlling this screen."
        case .closed: return "The remote session closed."
        case .hostReplaced: return "Another Nanocodex instance is sharing this screen."
        case .screenPermission: return "Allow Screen Recording for Nanocodex in System Settings."
        case .inputPermission: return "Allow Accessibility for Nanocodex in System Settings."
        case .geometryChanged: return "The screen size or orientation changed. Start sharing again to use its new layout."
        case .phoneBridge: return "The iPhone bridge could not start. Check the paired device, Developer Mode, and signed WebDriverAgent build."
        }
    }
}

/// Coordinates refer to the complete captured surface, before local letterboxing.
/// Discrete pointer events carry their own position so they cannot overtake motion.
public struct RemoteInput: Codable, Equatable, Sendable {
    public enum Kind: String, Codable, Sendable { case move, button, scroll, key, text, releaseAll }
    public let kind: Kind
    public let sequence: UInt64
    public let generation: String
    public var x: Double?
    public var y: Double?
    public var button: Int?
    public var down: Bool?
    public var key: UInt16?
    public var text: String?
    public var deltaX: Double?
    public var deltaY: Double?

    public init(kind: Kind, sequence: UInt64, generation: String, x: Double? = nil, y: Double? = nil,
                button: Int? = nil, down: Bool? = nil, key: UInt16? = nil, text: String? = nil,
                deltaX: Double? = nil, deltaY: Double? = nil) {
        self.kind = kind; self.sequence = sequence; self.generation = generation
        self.x = x; self.y = y; self.button = button; self.down = down; self.key = key
        self.text = text; self.deltaX = deltaX; self.deltaY = deltaY
    }

    public func validate() throws {
        guard sequence > 0, sequence <= 9_007_199_254_740_991,
              !generation.isEmpty, generation.utf8.count <= 128 else { throw RemoteError.invalidMessage }
        for coordinate in [x, y].compactMap({ $0 }) {
            guard coordinate.isFinite, (0...1).contains(coordinate) else { throw RemoteError.invalidMessage }
        }
        switch kind {
        case .move:
            guard x != nil, y != nil, button == nil, down == nil, key == nil, text == nil,
                  deltaX == nil, deltaY == nil else { throw RemoteError.invalidMessage }
        case .button:
            guard x != nil, y != nil, let button, (0...2).contains(button), down != nil,
                  key == nil, text == nil, deltaX == nil, deltaY == nil else { throw RemoteError.invalidMessage }
        case .scroll:
            guard x != nil, y != nil, let deltaX, let deltaY, deltaX.isFinite, deltaY.isFinite,
                  abs(deltaX) <= 4096, abs(deltaY) <= 4096,
                  button == nil, down == nil, key == nil, text == nil else { throw RemoteError.invalidMessage }
        case .key:
            guard let key, RemoteKey.supported(key), down != nil, x == nil, y == nil, button == nil,
                  text == nil, deltaX == nil, deltaY == nil else { throw RemoteError.invalidMessage }
        case .text:
            guard let text, !text.isEmpty, text.utf8.count <= 4096, !text.contains("\0"),
                  x == nil, y == nil, button == nil, down == nil, key == nil,
                  deltaX == nil, deltaY == nil else { throw RemoteError.invalidMessage }
        case .releaseAll:
            guard x == nil, y == nil, button == nil, down == nil, key == nil, text == nil,
                  deltaX == nil, deltaY == nil else { throw RemoteError.invalidMessage }
        }
    }

    public static func decode(_ data: Data) throws -> Self {
        guard data.count <= 8192,
              let fields = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              Set(fields.keys).isSubset(of: ["kind", "sequence", "generation", "x", "y", "button", "down", "key", "text", "deltaX", "deltaY"])
        else { throw RemoteError.invalidMessage }
        let event = try JSONDecoder().decode(Self.self, from: data)
        try event.validate()
        return event
    }
}

/// Lives at the input injector, including when several agents share one hand.
/// A new control generation never inherits keys, buttons, or queued input.
public struct RemoteControlLease: Sendable {
    public private(set) var owner: String?
    public private(set) var generation: String?
    private var deadline: TimeInterval = 0
    private var lastMotion: UInt64 = 0
    private var lastDiscrete: UInt64 = 0
    public let duration: TimeInterval

    public init(duration: TimeInterval = 10) { self.duration = duration }

    public mutating func acquire(owner: String, generation: String, now: TimeInterval) throws {
        guard !owner.isEmpty, !generation.isEmpty, duration.isFinite, duration > 0 else { throw RemoteError.invalidMessage }
        guard self.owner == nil || now >= deadline else { throw RemoteError.busy }
        self.owner = owner; self.generation = generation; deadline = now + duration
        lastMotion = 0; lastDiscrete = 0
    }

    public mutating func renew(owner: String, generation: String, now: TimeInterval) throws {
        guard self.owner == owner, self.generation == generation, now < deadline else { throw RemoteError.unauthorized }
        deadline = now + duration
    }

    public func isExpired(now: TimeInterval) -> Bool { owner != nil && now >= deadline }

    public mutating func accept(_ event: RemoteInput, from owner: String, now: TimeInterval) throws -> Bool {
        try event.validate()
        guard self.owner == owner, generation == event.generation, now < deadline else { throw RemoteError.unauthorized }
        if event.kind == .move {
            // Clicks carry positions and establish a barrier against late motion.
            guard event.sequence > max(lastMotion, lastDiscrete) else { return false }
            lastMotion = event.sequence
        } else {
            guard event.sequence > lastDiscrete else { return false }
            lastDiscrete = event.sequence
        }
        return true
    }

    public mutating func release() {
        owner = nil; generation = nil; deadline = 0; lastMotion = 0; lastDiscrete = 0
    }
}

public struct RemoteSurface: Codable, Equatable, Identifiable, Sendable {
    public enum Kind: String, Codable, Sendable { case desktop, window, phone, vm }
    public let id: String
    public let name: String
    public let kind: Kind
    public let width: Int
    public let height: Int
    public let controllable: Bool
    public let agentTools: Bool?

    enum CodingKeys: String, CodingKey { case id, name, kind, width, height, controllable; case agentTools = "agent_tools" }

    public init(id: String, name: String, kind: Kind, width: Int, height: Int, controllable: Bool, agentTools: Bool? = nil) {
        self.id = id; self.name = name; self.kind = kind
        self.width = width; self.height = height; self.controllable = controllable
        self.agentTools = agentTools
    }
}
