import SwiftUI
import NanocodexUI
import NanocodexRemote

// Reuse the native rendering contracts directly. These names do not introduce a
// second renderer, copy media, or change the host's URL and paste handling.
public typealias EmbedMarkdown = ChatMarkdown
#if os(iOS)
public typealias EmbedComposerEditor = ChatComposerEditor
#endif
public typealias EmbedGeneratedOutput = ChatGeneratedOutput
public typealias EmbedGeneratedOutputView = ChatGeneratedOutputView
public typealias EmbedImageAttachment = ChatImageAttachment
public typealias EmbedLatestScreen = ChatLatestScreen
public typealias EmbedRemoteService = RemoteService
public typealias EmbedScreenSelection = RemoteScreenSelection

/// Conversation-scoped passive live screen. Identity changes tear down the old
/// viewer; expanding and collapsing the same conversation retain its transport.
/// Authentication and the separate interactive controls remain host-owned.
public struct EmbedLiveScreen: View {
    private let conversationID: String
    private let service: RemoteService
    @Binding private var selection: RemoteScreenSelection?
    @Binding private var expanded: Bool
    private let onClose: () -> Void
    private let onControls: (RemoteScreenSelection?) -> Void

    public init(conversationID: String, service: RemoteService,
                selection: Binding<RemoteScreenSelection?>, expanded: Binding<Bool>,
                onClose: @escaping () -> Void,
                onControls: @escaping (RemoteScreenSelection?) -> Void) {
        self.conversationID = conversationID
        self.service = service
        _selection = selection
        _expanded = expanded
        self.onClose = onClose
        self.onControls = onControls
    }

    public var body: some View {
        RemoteThreadScreen(service: service, selection: $selection, expanded: $expanded,
                           onClose: onClose, onControls: onControls)
            .id(conversationID)
    }
}
