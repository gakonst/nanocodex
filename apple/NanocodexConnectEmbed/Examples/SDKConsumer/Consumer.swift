import SwiftUI
import NanocodexConnectEmbed

/// Local presentation example. Replace these rows/actions with your authorized
/// managed-agent or Connect transport; this example does not contact a service.
@main
struct SDKConsumerApp: App {
    var body: some Scene { WindowGroup { ConsumerConversation() } }
}

private struct Message: Identifiable {
    let id = UUID().uuidString
    let text: String
}

private struct ConsumerConversation: View {
    @State private var messages = [Message(text: "This is a local Swift SDK example.")]
    @State private var followsLatest = true
    @State private var draft = ""

    var body: some View {
        NavigationStack {
            EmbedConversation(conversationID: "local-example", rows: messages.map { message in
                EmbedConversationRow(id: message.id, revision: message.text) {
                    Text(message.text).frame(maxWidth: .infinity, alignment: .leading)
                }
            }, followsLatest: $followsLatest,
               layout: .init(horizontalPadding: 20, rowSpacing: 16, verticalPadding: 16)) {
                HStack(alignment: .bottom) {
                    TextField("Message", text: $draft, axis: .vertical).lineLimit(1...5)
                    Button("Add") {
                        messages.append(Message(text: draft))
                        draft = ""
                        followsLatest = true
                    }
                    .disabled(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }.padding()
            }
            .navigationTitle("Swift SDK example")
        }
    }
}
