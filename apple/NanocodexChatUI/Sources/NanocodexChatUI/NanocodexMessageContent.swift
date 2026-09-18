import SwiftUI
import InboxCore
import NanocodexUI
#if os(macOS)
import AppKit
#else
import UIKit
#endif

/// Host-parsed captured context, rendered with the mobile conversation's typography.
public struct NanocodexCapturedMessageContext {
    public struct Capture {
        public var source: String
        public var sender: String
        public var text: String
        public var url: String
        public init(source: String, sender: String = "", text: String, url: String = "") {
            self.source = source; self.sender = sender; self.text = text; self.url = url
        }
    }
    public var request: String
    public var captures: [Capture]
    public init(request: String, captures: [Capture]) {
        self.request = request; self.captures = captures
    }
}

/// The mobile message layout. Supply delivery, steering and media controls in their
/// original order through `extraContent`, inside the message bubble.
public struct NanocodexMessageContent<ExtraContent: View>: View {
    private let row: TranscriptRow
    private let capturedContext: NanocodexCapturedMessageContext?
    private let extraContent: ExtraContent

    public init(row: TranscriptRow, capturedContext: NanocodexCapturedMessageContext? = nil,
                @ViewBuilder extraContent: () -> ExtraContent) {
        self.row = row; self.capturedContext = capturedContext; self.extraContent = extraContent()
    }

    public var body: some View {
        HStack(alignment: .top, spacing: 0) {
            if row.role == "You" { Spacer(minLength: 44) }
            VStack(alignment: .leading, spacing: 10) {
                if row.role == "Thinking" {
                    DisclosureGroup {
                        ChatMarkdown(text: row.text)
                    } label: {
                        if row.running { ProgressView().controlSize(.mini).accessibilityLabel("Thinking") }
                        else { Text("Thought process") }
                    }.font(.system(size: 13)).foregroundStyle(NanocodexConversationPalette.muted)
                } else if row.role == "You", let content = capturedContext {
                    Text(content.request).font(.body).lineSpacing(3).textSelection(.enabled)
                    DisclosureGroup("Captured context (\(content.captures.count))") {
                        ForEach(Array(content.captures.enumerated()), id: \.offset) { _, capture in
                            VStack(alignment: .leading, spacing: 4) {
                                Text(capture.source + (capture.sender.isEmpty ? "" : " · " + capture.sender)).font(.caption.weight(.semibold))
                                Text(capture.text.isEmpty ? capture.url : capture.text).font(.subheadline).textSelection(.enabled)
                            }.padding(.vertical, 4)
                        }
                    }.font(.caption).foregroundStyle(NanocodexConversationPalette.muted)
                } else if row.role == "Agent", !row.text.isEmpty {
                    ChatMarkdown(text: row.text, compact: true)
                } else if !row.text.isEmpty {
                    Text(row.text).font(.system(size: row.role == "Status" ? 14 : 17))
                        .lineSpacing(5).textSelection(.enabled)
                        .foregroundStyle(row.role == "Status" ? NanocodexConversationPalette.muted : NanocodexConversationPalette.text)
                }
                if !row.detail.isEmpty { Text(row.detail).font(.caption).foregroundStyle(NanocodexConversationPalette.muted) }
                extraContent
            }
            .accessibilityElement(children: .contain)
            .accessibilityLabel(row.role == "You" ? "Your message" : row.role == "Agent" ? "Assistant message" : row.role)
            .padding(.horizontal, row.role == "You" ? 12 : 0)
            .padding(.vertical, row.role == "You" || row.role == "Agent" ? 9 : 0)
            .background(row.role == "You" ? NanocodexConversationPalette.userMessage : Color.clear,
                        in: RoundedRectangle(cornerRadius: 18))
            .contextMenu {
                if row.role == "Agent", !row.text.isEmpty {
                    ChatCopyButton(text: row.text, showsLabel: true)
                }
            }
            .accessibilityAction(named: "Copy response") {
                #if os(macOS)
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(row.text, forType: .string)
                #else
                UIPasteboard.general.string = row.text
                #endif
            }
            if row.role != "You" { Spacer(minLength: row.role == "Agent" ? 16 : 0) }
        }.frame(maxWidth: .infinity, alignment: row.role == "You" ? .trailing : .leading)
    }
}

public extension NanocodexMessageContent where ExtraContent == EmptyView {
    init(row: TranscriptRow, capturedContext: NanocodexCapturedMessageContext? = nil) {
        self.init(row: row, capturedContext: capturedContext) { EmptyView() }
    }
}
