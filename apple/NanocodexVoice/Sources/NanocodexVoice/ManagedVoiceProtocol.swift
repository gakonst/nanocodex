import Foundation
import InboxCore
import NanocodexVoiceCore

public struct ManagedVoiceTranscript: Equatable, Sendable {
    public var speaker: String
    public var text: String
    public var isFinal: Bool
    public init(speaker: String, text: String, isFinal: Bool = true) {
        self.speaker = speaker; self.text = text; self.isFinal = isFinal
    }
}
public struct ManagedVoiceEffects: Equatable, Sendable {
    public var frames: [JSON] = []
    public var transcripts: [ManagedVoiceTranscript] = []
    public var status: String?
    public var terminate: String?
    public var reconnectAfterMS: Int?
    public var acknowledgeFrames = false
    public var scheduleFlush = false
    public var playbackEnabled: Bool?
    public var ready = false
    public var inputGeneration: UInt64?
    public var undeliveredAnswers: [String] = []
    public init() {}
}
public struct ManagedVoiceDelegation: Equatable, Sendable {
    public let id: String
    public let formattedInput: String
}
public struct ManagedVoiceUpdate: Equatable, Sendable {
    public var effects = ManagedVoiceEffects()
    public var delegation: ManagedVoiceDelegation?
    public var prefetch: ManagedVoicePrefetch?
    public init() {}
}
public struct ManagedVoicePrefetch: Equatable, Sendable {
    public let query: String
    public let debounceMS: Int
}

/// Thin native binding to the same Rust core used by WASM.
/// The Rust handle serializes access; Swift owns only ABI conversion and lifetime.
public final class ManagedVoiceProtocol: @unchecked Sendable {
    private let handle: UInt64
    public static var voices: [String] { staticCommand(["op": .string("catalog")]).array.map(\.string) }

    public init(voice: String = "cove", settings: VoiceSettings? = nil) throws {
        let bytes = Array(voice.utf8)
        handle = bytes.withUnsafeBufferPointer { nc_voice_create($0.baseAddress, $0.count) }
        guard handle != 0 else { throw ManagedError.invalidResponse }
        if let settings { _ = try command(["op": .string("configure"), "settings": settings.json]) }
    }
    deinit { nc_voice_destroy(handle) }
    public static func session(instructions: String, settings: VoiceSettings) throws -> JSON {
        try Self(settings: settings).command(["op": .string("session"), "instructions": .string(instructions)])
    }
    public func appendSpeech(_ text: String) throws -> ManagedVoiceEffects {
        try effects(command(["op": .string("speech"), "text": .string(text)]))
    }
    public func appendContext(_ text: String) throws -> ManagedVoiceEffects {
        try effects(command(["op": .string("append_context"), "text": .string(text)]))
    }
    public func appendText(_ text: String, role: String = "user") throws -> ManagedVoiceEffects {
        try effects(command(["op": .string("text"), "role": .string(role), "text": .string(text)]))
    }

    public static func sessionID() -> String {
        var value = UUID().uuid
        var bytes = withUnsafeBytes(of: &value) { Array($0) }
        var timestamp = UInt64(Date().timeIntervalSince1970 * 1_000)
        for index in stride(from: 5, through: 0, by: -1) { bytes[index] = UInt8(timestamp & 255); timestamp >>= 8 }
        bytes[6] = (bytes[6] & 15) | 0x70; bytes[8] = (bytes[8] & 63) | 0x80
        return bytes.enumerated().map { ([4, 6, 8, 10].contains($0.offset) ? "-" : "") + String(format: "%02x", $0.element) }.joined()
    }

    public static func instructions(context: JSON = .null) -> String {
        staticCommand(["op": .string("instructions"), "context": context]).string
    }
    static func startupContextFrames(_ context: JSON) -> [JSON] {
        staticCommand(["op": .string("startup_context"), "context": context]).array
    }
    static func delegation(input: String, transcript: [ManagedVoiceTranscript], tail: Bool = false) -> String {
        staticCommand(["op": .string("delegation"), "input": .string(input), "tail": .bool(tail),
            "transcript": .array(transcript.map { .object(["speaker": .string($0.speaker), "text": .string($0.text)]) })]).string
    }
    public func bindSession(_ id: String) { _ = try? command(["op": .string("bind"), "session_id": .string(id)]) }
    public func realtimeMessage(_ event: JSON) -> ManagedVoiceUpdate {
        do {
            let value = try command(["op": .string("realtime"), "event": event])
            var update = ManagedVoiceUpdate(); update.effects = try effects(value["effects"])
            if value["prefetch"] != .null {
                update.prefetch = .init(query: value["prefetch"]["query"].string,
                                        debounceMS: Int(value["prefetch"]["debounce_ms"].number))
            }
            if value["delegation"] != .null {
                update.delegation = .init(id: value["delegation"]["id"].string, formattedInput: value["delegation"]["formatted_input"].string)
            }
            return update
        } catch { var update = ManagedVoiceUpdate(); update.effects = failure(); return update }
    }
    public func agentEvent(_ event: JSON) -> ManagedVoiceEffects { apply(["op": .string("agent"), "event": event]) }
    public func managedEvent(_ event: JSON, cursor: String) -> ManagedVoiceEffects {
        apply(["op": .string("managed"), "envelope": .object(["event": event, "cursor": .string(cursor)])])
    }
    public func context(_ text: String) -> ManagedVoiceEffects { apply(["op": .string("context"), "text": .string(text)]) }
    public func flush(final: Bool = false) -> ManagedVoiceEffects { apply(["op": .string("flush"), "final": .bool(final)]) }
    public func takeTranscriptTail() -> String? {
        guard let tail = try? command(["op": .string("tail")]), tail != .null else { return nil }
        return tail.string
    }
    public func noteTypedInput() -> ManagedVoiceEffects { apply(["op": .string("typed_input")]) }
    public func closeEffects() -> ManagedVoiceEffects { apply(["op": .string("close")]) }
    public func sidebandOpened() -> ManagedVoiceEffects { apply(["op": .string("opened")]) }
    public func sidebandClosed(connectedMS: Int) -> ManagedVoiceEffects { apply(["op": .string("closed"), "connected_ms": .number(Double(max(0, connectedMS)))]) }
    public func framesSent(_ count: Int) { _ = try? command(["op": .string("ack"), "count": .number(Double(max(0, count)))]) }

    private func command(_ value: [String: JSON]) throws -> JSON {
        let bytes = Array(try JSONEncoder().encode(JSON.object(value)))
        let pointer = bytes.withUnsafeBufferPointer { nc_voice_apply(handle, $0.baseAddress, $0.count) }
        guard let pointer else { throw ManagedError.invalidResponse }
        defer { nc_voice_string_free(pointer) }
        let reply = try JSONDecoder().decode(JSON.self, from: Data(String(cString: pointer).utf8))
        guard reply["error"] == .null else { throw ManagedError.invalidResponse }
        return reply["value"]
    }
    private static func staticCommand(_ value: [String: JSON]) -> JSON { (try? Self().command(value)) ?? .null }
    private func apply(_ value: [String: JSON]) -> ManagedVoiceEffects {
        do { return try effects(command(value)) } catch { return failure() }
    }
    private func failure() -> ManagedVoiceEffects {
        var effects = ManagedVoiceEffects(); effects.terminate = "Voice protocol failed. Please reconnect."; return effects
    }
    private func effects(_ value: JSON) throws -> ManagedVoiceEffects {
        var effects = ManagedVoiceEffects()
        effects.frames = try value["frames"].array.map { try JSONDecoder().decode(JSON.self, from: Data($0.string.utf8)) }
        effects.transcripts = value["transcripts"].array.map { .init(speaker: $0["speaker"].string, text: $0["text"].string, isFinal: !$0["is_partial"].bool) }
        effects.ready = value["ready"].bool
        effects.inputGeneration = value["input_generation"] == .null ? nil : UInt64(value["input_generation"].number)
        effects.undeliveredAnswers = value["undelivered_answers"].array.map(\.string)
        effects.status = value["status"] == .null ? nil : value["status"].string
        effects.terminate = value["terminate"] == .null ? nil : value["terminate"].string
        effects.reconnectAfterMS = value["reconnect_after_ms"] == .null ? nil : Int(value["reconnect_after_ms"].number)
        effects.acknowledgeFrames = value["acknowledge_frames"].bool
        effects.scheduleFlush = value["schedule_flush"].bool
        effects.playbackEnabled = value["playback_enabled"] == .null ? nil : value["playback_enabled"].bool
        return effects
    }
}
