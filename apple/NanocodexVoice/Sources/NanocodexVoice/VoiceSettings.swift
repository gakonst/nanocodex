import Foundation
import InboxCore

/// Preferences for the ChatGPT subscription voice connection.
/// Rust validates these values and translates them to provider instructions and events.
public struct VoiceSettings: Codable, Equatable, Sendable {
    public enum OutputProvider: String, Codable, CaseIterable, Sendable { case openai, elevenlabs }
    public enum Pace: String, Codable, CaseIterable, Sendable { case slow, natural, fast }
    public enum Updates: String, Codable, CaseIterable, Sendable { case auto, results, silent }
    public enum HandoffMode: String, Codable, Sendable { case thinking, commentary, bemTags = "bem_tags" }
    public var outputProvider: OutputProvider?
    public var elevenLabsVoiceId: String?
    public var voice: String
    public var instructions: String
    public var pace: Pace
    public var updates: Updates
    public var handoffMode: HandoffMode
    public var acknowledgements: Bool?

    public init(voice: String = "cove", instructions: String = "", pace: Pace = .natural,
                updates: Updates = .auto, handoffMode: HandoffMode = .thinking, acknowledgements: Bool? = nil,
                outputProvider: OutputProvider? = nil, elevenLabsVoiceId: String? = nil) {
        self.outputProvider = outputProvider; self.elevenLabsVoiceId = elevenLabsVoiceId
        self.voice = voice; self.instructions = instructions; self.pace = pace
        self.updates = updates; self.handoffMode = handoffMode; self.acknowledgements = acknowledgements
    }

    private enum CodingKeys: String, CodingKey {
        case voice, instructions, pace, updates, handoffMode, acknowledgements, outputProvider, elevenLabsVoiceId
    }
    public init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        voice = try values.decodeIfPresent(String.self, forKey: .voice) ?? "cove"
        instructions = try values.decodeIfPresent(String.self, forKey: .instructions) ?? ""
        pace = try values.decodeIfPresent(Pace.self, forKey: .pace) ?? .natural
        updates = try values.decodeIfPresent(Updates.self, forKey: .updates) ?? .auto
        handoffMode = try values.decodeIfPresent(HandoffMode.self, forKey: .handoffMode) ?? .thinking
        acknowledgements = try values.decodeIfPresent(Bool.self, forKey: .acknowledgements)
        outputProvider = try values.decodeIfPresent(OutputProvider.self, forKey: .outputProvider)
        elevenLabsVoiceId = try values.decodeIfPresent(String.self, forKey: .elevenLabsVoiceId)
    }

    var json: JSON { get throws { try JSONDecoder().decode(JSON.self, from: JSONEncoder().encode(self)) } }
    static func load() -> Self {
        if let data = UserDefaults.standard.data(forKey: "nanocodex.voice.settings"),
           let value = try? JSONDecoder().decode(Self.self, from: data),
           (try? ManagedVoiceProtocol(settings: value)) != nil { return value }
        let previous = UserDefaults.standard.string(forKey: "nanocodex.voice") ?? "cove"
        return Self(voice: ManagedVoiceProtocol.voices.contains(previous) ? previous : "cove")
    }
    func save() {
        if let data = try? JSONEncoder().encode(self) { UserDefaults.standard.set(data, forKey: "nanocodex.voice.settings") }
    }
}
