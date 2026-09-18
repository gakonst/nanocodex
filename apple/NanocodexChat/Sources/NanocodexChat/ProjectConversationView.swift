import SwiftUI
import InboxCore

/// A foreground lifecycle boundary around a project conversation store. The
/// host supplies the complete layout through a view builder. Use one lifecycle
/// owner per store; sidebar and transcript children read that same instance.
///
/// Create a new store and give this view a new identity when the signed-in
/// account or approved project grant changes. The host owns login, consent and
/// credential storage; none of those concerns enter the view hierarchy here.
@MainActor public struct ProjectConversationView<Content: View>: View {
    @ObservedObject private var store: ProjectConversationStore
    private let content: (ProjectConversationStore) -> Content

    public init(store: ProjectConversationStore,
                @ViewBuilder content: @escaping (ProjectConversationStore) -> Content) {
        self.store = store; self.content = content
    }

    public var body: some View {
        ProjectConversationLifecycle(store: store, content: content)
            .id(ObjectIdentifier(store))
    }
}

@MainActor private struct ProjectConversationLifecycle<Content: View>: View {
    @ObservedObject var store: ProjectConversationStore
    @Environment(\.scenePhase) private var scenePhase
    let content: (ProjectConversationStore) -> Content

    var body: some View {
        content(store)
            .task(id: scenePhase) {
                if scenePhase == .active { await store.resume() }
                else { store.suspend() }
            }
            .onDisappear { store.suspend() }
    }
}

public extension ProjectConversationStore {
    /// Captures the conversation identity when the view renders, so an outgoing
    /// editor cannot write into the newly selected conversation during a switch.
    var draftBinding: Binding<String> {
        let id = selection
        return Binding(
            get: { id.flatMap { self.drafts[$0] } ?? "" },
            set: { text in if let id { self.setDraft(text, for: id) } }
        )
    }
}
