import Foundation

/// Bound source before SwiftUI measures or highlights a collapsed tool card.
/// The original source stays with the tool for explicit viewing and copying.
public struct ChatCodePreview: Equatable, Sendable {
    public let text: String
    public let isTruncated: Bool

    public init(_ source: String, maximumCharacters: Int = 512, maximumLines: Int = 8) {
        let characters = max(1, maximumCharacters)
        let lines = max(1, maximumLines)
        let prefix = source.prefix(characters)
        var line = 1
        var end = prefix.endIndex
        for index in prefix.indices where prefix[index].isNewline {
            if line == lines { end = index; break }
            line += 1
        }
        text = String(prefix[..<end])
        isTruncated = text != source
    }
}
