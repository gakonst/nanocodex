import Foundation

/// Prepare history-sized publication work away from the UI executor. The caller
/// still validates account/observation identity and cancellation before publishing.
public struct TranscriptPublicationPreparation: Sendable {
    public let card: AgentCard?
    public let rowsChanged: Bool
    private let baseCard: AgentCard?
    private let rowsRevision: UUID

    public init(events: [AgentEvent], rows: [TranscriptRow], previousRows: [TranscriptRow],
                card: AgentCard?, rowsRevision: UUID) {
        baseCard = card
        self.rowsRevision = rowsRevision
        rowsChanged = previousRows != rows
        var prepared = card
        prepared?.apply(events: events, transcriptRows: rows)
        self.card = prepared
    }

    /// A refresh or navigation during asynchronous preparation must not be
    /// overwritten by this snapshot. New stream events alone may still publish
    /// coherent progress while the next projection catches up.
    public func isCurrent(rowsRevision: UUID, card: AgentCard?) -> Bool {
        self.rowsRevision == rowsRevision && baseCard == card
    }
}
