import SwiftUI
import InboxCore

/// Unstyled navigation content. The host owns scrolling, spacing, search,
/// selection actions and presentation (sidebar, drawer or navigation stack).
/// Pass only the active project's authorized roster. This view does not confer
/// authorization and never fetches an account-wide roster.
public struct ConversationListContent<Row: View, Empty: View>: View {
    private let cards: [AgentCard]
    private let row: (AgentCard) -> Row
    private let empty: () -> Empty

    public init(cards: [AgentCard], @ViewBuilder row: @escaping (AgentCard) -> Row,
                @ViewBuilder empty: @escaping () -> Empty) {
        self.cards = cards; self.row = row; self.empty = empty
    }

    public var body: some View {
        ForEach(cards) { card in row(card) }
        if cards.isEmpty { empty() }
    }
}

/// Compact conversation content: user/final messages remain visible while the
/// caller renders each turn's tool/commentary activity as a disclosure, timeline
/// or another presentation. No fonts, bubbles, padding, backgrounds, scrolling,
/// disclosure state or automatic scrolling are imposed by this component.
public struct ConversationTranscriptContent<Message: View, Activity: View>: View {
    private let items: [ConversationItem]
    private let message: (TranscriptRow) -> Message
    private let activity: (ConversationItem) -> Activity

    public init(items: [ConversationItem],
                @ViewBuilder message: @escaping (TranscriptRow) -> Message,
                @ViewBuilder activity: @escaping (ConversationItem) -> Activity) {
        self.items = items; self.message = message; self.activity = activity
    }

    public var body: some View {
        ForEach(items) { item in
            Group {
                if let row = item.message { message(row) }
                else { activity(item) }
            }.id(item.id)
        }
    }
}
