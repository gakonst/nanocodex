import Foundation

/// Chronological feed entries retain the identity and position of their source
/// row. Tool completion updates its card in place; turns never reorder the feed.
public struct ConversationItem: Identifiable, Equatable {
    public let id: String
    public var message: TranscriptRow?
    public var activity: [TranscriptRow] = []
    public var isRunning = false

    public static func group(_ rows: [TranscriptRow], activeTurns: [String] = []) -> [ConversationItem] {
        rows.map { row in
            let live = row.running && row.turnID != nil && row.turnID == activeTurns.first
            if row.role == "Tool" {
                return .init(id: row.id, activity: [row], isRunning: live)
            }
            return .init(id: row.id, message: row, isRunning: live)
        }
    }
}
