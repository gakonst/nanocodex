#if DEBUG && targetEnvironment(simulator)
import SwiftUI
import NanocodexConnectEmbed

/// A real public-SDK consumer with deterministic host updates and no transport.
/// Exercises both rendering paths on the same simulator through the public option.
struct EmbedConversationUIFixture: View {
    @State private var conversation = "First"
    @State private var revision = 0
    @State private var draft = ""
    @State private var followsLatest = true
    @State private var sent: [String] = []
    @State private var mounted = true
    @State private var allowSend = false
    @State private var historyStart = 0
    private var historyJourney: Bool {
        ProcessInfo.processInfo.arguments.contains("--embed-history")
    }
    private var rendering: EmbedTranscriptRendering {
        ProcessInfo.processInfo.arguments.contains("--embed-scroll-view") ? .scrollView : .automatic
    }

    var body: some View {
        VStack {
            HStack {
                if historyJourney {
                    Button("Prepend history") { historyStart -= 20 }
                    Text("History: \(40 - historyStart)").accessibilityIdentifier("embed-history-count")
                }
                Button("Switch conversation") {
                    conversation = conversation == "First" ? "Second" : "First"
                    revision = 0
                    sent = []
                    followsLatest = true
                }
                Button(mounted ? "Unmount" : "Mount") { mounted.toggle() }
            }
            if mounted {
                EmbedConversation(conversationID: conversation, rows: rows,
                                  followsLatest: $followsLatest,
                                  layout: .init(horizontalPadding: 16, rowSpacing: 12, verticalPadding: 12),
                                  rendering: rendering, transcriptOverlay: {
                    if !followsLatest {
                        Button("Latest") { followsLatest = true }
                            .padding(16).frame(maxWidth: .infinity, alignment: .trailing)
                    }
                }, accessories: {
                    HStack {
                        Button("Stream update") { revision += 1 }
                        Text(followsLatest ? "Following" : "Reading").accessibilityIdentifier("embed-following")
                    }
                }, composer: {
                    VStack {
                        Toggle("Allow send", isOn: $allowSend)
                        HStack {
                            TextField("Draft", text: $draft).accessibilityIdentifier("embed-draft")
                            Button("Send") {
                                sent.append(draft)
                                draft = ""
                                followsLatest = true
                            }
                            .disabled(!allowSend || draft.isEmpty)
                            .accessibilityIdentifier("embed-send")
                        }
                        Text("Submitted: \(sent.count)").accessibilityIdentifier("embed-submitted")
                    }.padding()
                }).accessibilityIdentifier("embed-surface")
            }
        }
    }

    private var rows: [EmbedConversationRow] {
        let name = conversation
        let version = revision
        if historyJourney {
            return [EmbedConversationRow(id: "header", countsAsMessage: false, revision: historyStart) {
                Text("Earlier messages")
            }] + (historyStart..<40).map { index in
                EmbedConversationRow(id: "history-\(index)", revision: index) {
                    Text("History message \(index)")
                        .frame(minHeight: 70)
                        .accessibilityIdentifier("embed-history-\(index)")
                }
            }
        }
        return [EmbedConversationRow(id: "reply", revision: "\(name)-\(version)") {
            EmbedFixtureReply(text: "\(name) reply \(version)")
        }] + sent.enumerated().map { index, message in
            EmbedConversationRow(id: "sent-\(index)", revision: message) { Text(message) }
        }
    }
}

private struct EmbedFixtureReply: View {
    let text: String
    @State private var expanded = false
    var body: some View {
        VStack(alignment: .leading) {
            Text(text).accessibilityIdentifier("embed-reply")
            Button("Details") { expanded.toggle() }
            if expanded { Text("Expanded detail").accessibilityIdentifier("embed-detail") }
        }
    }
}
#endif
