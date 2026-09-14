import Foundation

/// Projects protocol handoffs into spoken turns without changing durable/model input.
/// nil means ordinary text; an empty array means internal lifecycle/metadata only.
public enum RealtimeTranscript {
    public struct Turn: Equatable, Sendable {
        public let speaker: String
        public var text: String
    }

    public static func project(_ text: String, isPartial: Bool = false) -> [Turn]? {
        let envelope = text.trimmingCharacters(in: .whitespacesAndNewlines)
        // Streaming must not flash the opening of an internal envelope before
        // enough characters arrive to recognize its full tag.
        if isPartial, !envelope.isEmpty,
           ["<realtime_delegation", "<realtime_conversation", "<source", "<soruce", "<startup_context"].contains(where: { $0.hasPrefix(envelope) }) { return [] }
        if envelope.range(of: "^<(?:source|soruce|startup_context)(?:\\s|>|$)", options: .regularExpression) != nil { return [] }
        if envelope.range(of: "^<realtime_conversation(?:\\s|>|$)", options: .regularExpression) != nil { return [] }
        guard envelope.range(of: "^<realtime_delegation(?:\\s|>|$)", options: .regularExpression) != nil else { return nil }
        guard let encoded = field("transcript_delta", in: envelope), !encoded.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            // Tail input is a synthetic instruction, never the user's speech.
            guard !envelope.contains("<source>"), !envelope.contains("<soruce>"),
                  let input = field("input", in: envelope), !input.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return [] }
            return [Turn(speaker: "user", text: decode(input))]
        }
        var turns: [Turn] = []
        for line in decode(encoded).components(separatedBy: "\n") {
            if let speaker = ["user", "assistant"].first(where: { line.hasPrefix($0 + ":") }) {
                var value = String(line.dropFirst(speaker.count + 1))
                if value.hasPrefix(" ") { value.removeFirst() }
                turns.append(Turn(speaker: speaker, text: value))
            } else if !turns.isEmpty {
                turns[turns.count - 1].text += "\n" + line
            } else if !line.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                turns.append(Turn(speaker: "assistant", text: line))
            }
        }
        return turns.filter { !$0.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
    }

    private static func field(_ name: String, in text: String) -> String? {
        guard let start = text.range(of: "<\(name)>"),
              let end = text.range(of: "</\(name)>", range: start.upperBound..<text.endIndex) else { return nil }
        return String(text[start.upperBound..<end.lowerBound])
    }

    private static func decode(_ text: String) -> String {
        text.replacingOccurrences(of: "&lt;", with: "<")
            .replacingOccurrences(of: "&gt;", with: ">")
            .replacingOccurrences(of: "&quot;", with: "\"")
            .replacingOccurrences(of: "&apos;", with: "'")
            .replacingOccurrences(of: "&amp;", with: "&")
    }
}
