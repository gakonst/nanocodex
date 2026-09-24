import Foundation

public enum QuickVoiceInput {
    public static func matches(_ url: URL) -> Bool {
        url.scheme == "nanocodex" && url.host == "voice" && url.path == "/new"
            && url.query == nil && url.fragment == nil && url.user == nil && url.password == nil && url.port == nil
    }

    public static func finalText(_ text: String) -> String? {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }
}

/// Rejects late callbacks after cancellation, interruption, completion, or a language change.
public struct QuickVoiceCaptureGate {
    public private(set) var token = UUID()
    public private(set) var active = false
    public init() {}
    public mutating func begin() -> UUID {
        token = UUID(); active = true
        return token
    }
    public mutating func cancel() { token = UUID(); active = false }
    public func accepts(_ candidate: UUID) -> Bool { active && candidate == token }
    public mutating func completed(_ text: String, token candidate: UUID, isFinal: Bool) -> String? {
        guard accepts(candidate), isFinal, let text = QuickVoiceInput.finalText(text) else { return nil }
        cancel()
        return text
    }
}

/// Public, privacy-safe Live Activity labels for capture failures. Never forward
/// arbitrary AVFoundation or Speech errors to the Lock Screen.
public enum LockedVoiceFailure {
    public static func description(for message: String) -> String {
        switch message {
        case "Recording interrupted.": "Recording interrupted"
        case "Audio recording ended unexpectedly.": "Microphone stopped"
        case "Microphone could not start.", "Audio recording failed.", "No microphone is available.": "Microphone unavailable"
        case "Speech recognition stopped.", "Speech recognition is unavailable. Try again when connected.": "Speech recognition unavailable"
        case "Recording storage unavailable.": "Recording storage unavailable"
        case "Microphone or speech permission unavailable.": "Allow microphone and speech access"
        case "Another recording is in progress.", "Another voice recording is in progress. Finish it first.": "Another recording is active"
        case "No speech was recognized. Try again.": "No speech heard"
        default: "Recording stopped"
        }
    }
}
