# NanocodexChat

Unstyled SwiftUI conversation components for iOS 17+ and macOS 14+. `InboxCore` owns the shared state/transport; this package owns a foreground lifecycle boundary and view builders. There is no palette, font, bubble, navigation container or Markdown dependency.

Add the local `apple/NanocodexChat` package and its `NanocodexChat` product. Hosts also import `InboxCore` for store and transport types. Repository consumers can vendor these sibling packages; the repository root is not itself a Swift package.

## Connect a host

After the host finishes its existing Connect login, construct `ConnectConversationAuthorization` from the actual returned grant ID, token, agent ID, capabilities and registered app identity. This full-conversation component requires history and final-reply visibility (or trace visibility), so an ungranted history cannot masquerade as an empty conversation. The server still checks the live grant on every request. Pass it to `ConnectConversationTransport(authorization:title:)`, then `ProjectConversationStore(transport:)`. Keep that store for the lifetime of this authorization. Do not pass an account API key to a third-party app.

The current Connect server grants **one conversation per grant**. The scope uses its grant ID as a state namespace. This package does not turn that into multi-conversation server project authorization. The [component design](../../docs/ux/2026-09-18-swift-project-conversations.md) describes the additional server contract required for the full named “DJ Booth” project journey.

First-party apps can use `ProjectConversationManagedTransport(client:authorizedScope:)` with their existing account client. Its roster restricts UI operations; the account credential still has its server-granted authority. Custom backends implement `ProjectConversationTransport` and enforce project membership on each operation.

## Supply the views

One `ProjectConversationView` owns foreground observation for a store. It automatically suspends observation when the scene becomes inactive or the view disappears. Replacing the store resets that lifecycle boundary. Login, credential storage, project selection and sign-out belong to the host.

```swift
import SwiftUI
import InboxCore
import NanocodexChat

struct ProjectChat: View {
    let store: ProjectConversationStore

    var body: some View {
        ProjectConversationView(store: store) { state in
            HStack(alignment: .top) {
                ScrollView {
                    LazyVStack {
                        ConversationListContent(cards: state.cards) { card in
                            Button(card.title) {
                                Task { await state.select(card.id) }
                            }
                        } empty: {
                            Text("No conversations")
                        }
                    }
                }
                VStack {
                    ScrollView {
                        LazyVStack(alignment: .leading) {
                            ConversationTranscriptContent(items: state.items) { row in
                                Text(row.text) // Supply your Markdown/media view here.
                            } activity: { item in
                                DisclosureGroup(item.isRunning ? "Working" : "Activity") {
                                    ForEach(item.activity) { Text($0.text) }
                                }
                            }
                        }
                    }
                    TextField("Message", text: state.draftBinding)
                    Button("Send") { Task { await state.send() } }
                        .disabled(state.selection == nil)
                }
            }
        }
    }
}
```

This example deliberately leaves error/pending/loading views and scrolling policy to the host. A shipping host must display `error`, `isLoading`, `connection`, and the selected conversation's `pending` entry. Pending input stays separate from durable transcript rows. Offer `retryPending(for:)` for an uncertain send; it reuses the captured request and input. `stop()` captures the selected agent and queue head before awaiting the request. Do not cancel a server turn when a view disappears.

Use `ConversationRoster` for stable sidebar ordering and search without moving rows beneath the pointer during streaming. Place `ConversationListContent` inside the host's own lazy stack/list/drawer. `ConversationTranscriptContent` provides stable row identities and compact per-turn activity grouping; the host owns disclosure state, generated media rendering and reading-position policy.

`loadOlder()` pauses live observation while reading a bounded older window. `jumpToLatest()` returns to the newest history and live stream. The host should provide an explicit latest-messages action when browsing history. Cursor-only SSE checkpoints remain transport state and never render as messages.

The first slice supports text follow-ups and stopping a turn. Multi-agent project creation, uploads, steering, interactive tool approvals, voice, durable draft/outbox persistence and the complete mobile-model migration remain integration work. Keep the existing mobile model as its sole transcript owner until those surfaces migrate; do not subscribe both models to the same UI.

## Validation

Run `swift test --package-path apple/InboxCore` and `swift test --package-path apple/NanocodexChat` from the repository root. Tests exercise project boundaries, grant routes, state/history races, stream cursors, explicit retry identity and outgoing composer bindings. The mobile drawer consumes the shared roster/list primitive; its existing simulator tests cover switching and independent drafts.
