import XCTest
import SwiftUI
import InboxCore
@testable import NanocodexChat

@MainActor final class ConversationBindingTests: XCTestCase {
    private final class Transport: ProjectConversationTransport {
        let scope = ProjectConversationScope(projectID: "test-project", conversations: [
            .init(id: "first", title: "First"), .init(id: "second", title: "Second")
        ])
        func state(_ id: String) async throws -> JSON {
            .object(["agent_id": .string(id), "latest_event_cursor": .string("0"), "active_turns": .array([])])
        }
        func history(_ id: String, before: Cursor?, after: Cursor?) async throws -> EventPage {
            try EventPage(.object(["data": .array([]), "latest_cursor": .string("0"), "has_more": .bool(false)]))
        }
        func stream(_ id: String, after: Cursor, receive: @escaping @Sendable (ProjectConversationFrame) async -> Void) async throws {
            throw APIError.http(403)
        }
        func send(_ command: AgentCommand) async throws { throw APIError.http(403) }
    }

    func testOutgoingComposerCannotEditNewSelection() async {
        let store = ProjectConversationStore(transport: Transport())
        await store.select("first")
        let outgoing = store.draftBinding
        outgoing.wrappedValue = "First draft"
        await store.select("second")
        let current = store.draftBinding
        current.wrappedValue = "Second draft"
        // SwiftUI can deliver an editor callback while removing the old view.
        outgoing.wrappedValue = "Late first edit"
        XCTAssertEqual(store.drafts["first"], "Late first edit")
        XCTAssertEqual(store.drafts["second"], "Second draft")
        XCTAssertEqual(current.wrappedValue, "Second draft")
        store.suspend()
    }

    func testBuildersAcceptHostDefinedLayoutAndContent() {
        let store = ProjectConversationStore(transport: Transport())
        // Compile a full consumer through public package APIs. There is no
        // dependency on NanocodexUI's palette, Markdown or app model.
        _ = ProjectConversationView(store: store) { state in
            HStack {
                ConversationListContent(cards: state.cards) { card in
                    Button(card.title) { Task { await state.select(card.id) } }
                } empty: { Text("No conversations") }
                VStack {
                    ConversationTranscriptContent(items: state.items) { row in
                        Text(row.text)
                    } activity: { item in
                        DisclosureGroup("Activity") { ForEach(item.activity) { Text($0.text) } }
                    }
                    TextField("Message", text: state.draftBinding)
                }
            }
        }
    }
}
