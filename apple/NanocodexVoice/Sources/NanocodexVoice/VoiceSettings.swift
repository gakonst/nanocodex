import Foundation
import InboxCore

/// Preferences for the ChatGPT subscription voice connection.
/// Rust validates these values and translates them to provider instructions and events.
public struct VoiceSettings: Codable, Equatable, Sendable {
    public enum Pace: String, Codable, CaseIterable, Sendable { case slow, natural, fast }
    public enum Updates: String, Codable, CaseIterable, Sendable { case auto, results, silent }
    public enum HandoffMode: String, Codable, Sendable { case thinking, commentary, bemTags = "bem_tags" }
    public var voice: String
    public var instructions: String
    public var pace: Pace
    public var updates: Updates
    public var handoffMode: HandoffMode
    public var acknowledgements: Bool?

    public init(voice: String = "cove", instructions: String = "", pace: Pace = .natural,
                updates: Updates = .auto, handoffMode: HandoffMode = .thinking, acknowledgements: Bool? = nil) {
        self.voice = voice; self.instructions = instructions; self.pace = pace
        self.updates = updates; self.handoffMode = handoffMode; self.acknowledgements = acknowledgements
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
