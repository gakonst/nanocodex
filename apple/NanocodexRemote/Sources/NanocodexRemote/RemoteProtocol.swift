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

/// Complete controller snapshot. A neutral snapshot releases every gamepad control.
public struct RemoteGamepadState: Codable, Equatable, Sendable {
    public var leftX: Double
    public var leftY: Double
    public var rightX: Double
    public var rightY: Double
    public var leftTrigger: Double
    public var rightTrigger: Double
    public var buttons: [String]

    public init(leftX: Double = 0, leftY: Double = 0, rightX: Double = 0, rightY: Double = 0,
                leftTrigger: Double = 0, rightTrigger: Double = 0, buttons: [String] = []) {
        self.leftX = leftX; self.leftY = leftY; self.rightX = rightX; self.rightY = rightY
        self.leftTrigger = leftTrigger; self.rightTrigger = rightTrigger; self.buttons = buttons
    }

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case leftX, leftY, rightX, rightY, leftTrigger, rightTrigger, buttons
    }
    private struct Field: CodingKey {
        let stringValue: String
        var intValue: Int? { nil }
        init?(stringValue: String) { self.stringValue = stringValue }
        init?(intValue: Int) { return nil }
    }

    public init(from decoder: Decoder) throws {
        let fields = try decoder.container(keyedBy: Field.self)
        guard Set(fields.allKeys.map(\.stringValue)) == Set(CodingKeys.allCases.map(\.rawValue)) else {
            throw RemoteError.invalidMessage
        }
        let values = try decoder.container(keyedBy: CodingKeys.self)
        leftX = try values.decode(Double.self, forKey: .leftX)
        leftY = try values.decode(Double.self, forKey: .leftY)
        rightX = try values.decode(Double.self, forKey: .rightX)
        rightY = try values.decode(Double.self, forKey: .rightY)
        leftTrigger = try values.decode(Double.self, forKey: .leftTrigger)
        rightTrigger = try values.decode(Double.self, forKey: .rightTrigger)
        buttons = try values.decode([String].self, forKey: .buttons)
        try validate()
    }

    public func validate() throws {
        guard [leftX, leftY, rightX, rightY].allSatisfy({ $0.isFinite && (-1...1).contains($0) }),
              [leftTrigger, rightTrigger].allSatisfy({ $0.isFinite && (0...1).contains($0) }),
              buttons.count <= 14, Set(buttons).count == buttons.count,
              Set(buttons).isSubset(of: ["a", "b", "x", "y", "dpadUp", "dpadDown", "dpadLeft", "dpadRight",
                  "leftShoulder", "rightShoulder", "leftStick", "rightStick", "back", "start"])
        else { throw RemoteError.invalidMessage }
    }
}

/// Coordinates refer to the complete captured surface, before local letterboxing.
/// Absolute pointer events carry positions; relative input uses the reliable channel.
public struct RemoteInput: Codable, Equatable, Sendable {
    public enum Kind: String, Codable, Sendable { case move, relativeMove, button, scroll, key, text, releaseAll, gamepad }
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
    public var gamepad: RemoteGamepadState?

    public init(kind: Kind, sequence: UInt64, generation: String, x: Double? = nil, y: Double? = nil,
                button: Int? = nil, down: Bool? = nil, key: UInt16? = nil, text: String? = nil,
                deltaX: Double? = nil, deltaY: Double? = nil, gamepad: RemoteGamepadState? = nil) {
        self.kind = kind; self.sequence = sequence; self.generation = generation
        self.x = x; self.y = y; self.button = button; self.down = down; self.key = key
        self.text = text; self.deltaX = deltaX; self.deltaY = deltaY; self.gamepad = gamepad
    }

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case kind, sequence, generation, x, y, button, down, key, text, deltaX, deltaY, gamepad
    }
    private struct Field: CodingKey {
        let stringValue: String
        var intValue: Int? { nil }
        init?(stringValue: String) { self.stringValue = stringValue }
        init?(intValue: Int) { return nil }
    }

    public init(from decoder: Decoder) throws {
        let fields = Set(try decoder.container(keyedBy: Field.self).allKeys.map(\.stringValue))
        guard fields.isSubset(of: Set(CodingKeys.allCases.map(\.rawValue))) else { throw RemoteError.invalidMessage }
        let values = try decoder.container(keyedBy: CodingKeys.self)
        kind = try values.decode(Kind.self, forKey: .kind)
        if kind == .gamepad {
            guard fields == ["kind", "sequence", "generation", "gamepad"] else { throw RemoteError.invalidMessage }
        } else if fields.contains("gamepad") { throw RemoteError.invalidMessage }
        sequence = try values.decode(UInt64.self, forKey: .sequence)
        generation = try values.decode(String.self, forKey: .generation)
        x = try values.decodeIfPresent(Double.self, forKey: .x)
        y = try values.decodeIfPresent(Double.self, forKey: .y)
        button = try values.decodeIfPresent(Int.self, forKey: .button)
        down = try values.decodeIfPresent(Bool.self, forKey: .down)
        key = try values.decodeIfPresent(UInt16.self, forKey: .key)
        text = try values.decodeIfPresent(String.self, forKey: .text)
        deltaX = try values.decodeIfPresent(Double.self, forKey: .deltaX)
        deltaY = try values.decodeIfPresent(Double.self, forKey: .deltaY)
        gamepad = try values.decodeIfPresent(RemoteGamepadState.self, forKey: .gamepad)
        try validate()
    }

    public func validate() throws {
        guard sequence > 0, sequence <= 9_007_199_254_740_991,
              !generation.isEmpty, generation.utf8.count <= 128 else { throw RemoteError.invalidMessage }
        for coordinate in [x, y].compactMap({ $0 }) {
            guard coordinate.isFinite, (0...1).contains(coordinate) else { throw RemoteError.invalidMessage }
        }
        guard kind == .gamepad || gamepad == nil else { throw RemoteError.invalidMessage }
        let point = x != nil && y != nil
        let noPoint = x == nil && y == nil
        switch kind {
        case .move:
            guard x != nil, y != nil, button == nil, down == nil, key == nil, text == nil,
                  deltaX == nil, deltaY == nil else { throw RemoteError.invalidMessage }
        case .button:
            guard (point || noPoint), let button, (0...2).contains(button), down != nil,
                  key == nil, text == nil, deltaX == nil, deltaY == nil else { throw RemoteError.invalidMessage }
        case .relativeMove, .scroll:
            guard (kind == .relativeMove ? noPoint : (point || noPoint)),
                  let deltaX, let deltaY, deltaX.isFinite, deltaY.isFinite,
                  abs(deltaX) <= 4096, abs(deltaY) <= 4096,
                  button == nil, down == nil, key == nil, text == nil else { throw RemoteError.invalidMessage }
        case .key:
            guard let key, RemoteKey.supported(key), down != nil, x == nil, y == nil, button == nil,
                  text == nil, deltaX == nil, deltaY == nil else { throw RemoteError.invalidMessage }
        case .text:
            guard let text, !text.isEmpty, text.utf8.count <= 4096, !text.contains("\0"),
                  x == nil, y == nil, button == nil, down == nil, key == nil,
                  deltaX == nil, deltaY == nil else { throw RemoteError.invalidMessage }
        case .gamepad:
            guard let gamepad, noPoint, button == nil, down == nil, key == nil, text == nil,
                  deltaX == nil, deltaY == nil else { throw RemoteError.invalidMessage }
            try gamepad.validate()
        case .releaseAll:
            guard x == nil, y == nil, button == nil, down == nil, key == nil, text == nil,
                  deltaX == nil, deltaY == nil else { throw RemoteError.invalidMessage }
        }
    }

    public static func decode(_ data: Data) throws -> Self {
        guard data.count <= 8192,
              let fields = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              Set(fields.keys).isSubset(of: ["kind", "sequence", "generation", "x", "y", "button", "down", "key", "text", "deltaX", "deltaY", "gamepad"])
        else { throw RemoteError.invalidMessage }
        if fields["kind"] as? String == "gamepad" {
            guard Set(fields.keys) == ["kind", "sequence", "generation", "gamepad"] else { throw RemoteError.invalidMessage }
        } else if fields.keys.contains("gamepad") { throw RemoteError.invalidMessage }
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
    public let broadcast: Bool?

    enum CodingKeys: String, CodingKey { case id, name, kind, width, height, controllable, broadcast; case agentTools = "agent_tools" }

    public init(id: String, name: String, kind: Kind, width: Int, height: Int, controllable: Bool, agentTools: Bool? = nil, broadcast: Bool? = nil) {
        self.id = id; self.name = name; self.kind = kind
        self.width = width; self.height = height; self.controllable = controllable
        self.agentTools = agentTools; self.broadcast = broadcast
    }
}
