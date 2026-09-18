import SwiftUI

/// Expanded message editing presentation. The host owns sending and dismissal.
public struct ChatExpandedComposer: View {
    @Binding private var draft: String
    let canSend: Bool
    let attachmentCount: Int
    let onCollapse: () -> Void
    let onSend: () -> Void
    private let background: Color
    @FocusState private var editorFocused: Bool

    public init(
        draft: Binding<String>,
        canSend: Bool,
        attachmentCount: Int,
        background: Color,
        onCollapse: @escaping () -> Void,
        onSend: @escaping () -> Void
    ) {
        _draft = draft
        self.canSend = canSend
        self.attachmentCount = attachmentCount
        self.background = background
        self.onCollapse = onCollapse
        self.onSend = onSend
    }

    public var body: some View {
        NavigationStack {
            VStack(alignment: .leading, spacing: 8) {
                TextEditor(text: $draft)
                    .font(.body)
                    .scrollContentBackground(.hidden)
                    .focused($editorFocused)
                    .accessibilityLabel("Message")
                    .accessibilityIdentifier("expanded-composer")
                    .overlay(alignment: .topLeading) {
                        if draft.isEmpty {
                            Text("Ask Nanocodex")
                                .foregroundStyle(.secondary)
                                .padding(.horizontal, 5).padding(.top, 8)
                                .allowsHitTesting(false)
                                .accessibilityHidden(true)
                        }
                    }
                if attachmentCount > 0 {
                    Label("Attachments: \(attachmentCount)", systemImage: "paperclip")
                        .font(.caption).foregroundStyle(.secondary)
                }
            }
            .padding(16)
            .background(background)
            .navigationTitle("Message")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(action: onCollapse) {
                        Label("Collapse", systemImage: "arrow.down.right.and.arrow.up.left")
                    }.accessibilityLabel("Collapse message editor")
                        .accessibilityIdentifier("collapse-composer")
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Send", action: onSend)
                        .disabled(!canSend)
                        .keyboardShortcut(.return, modifiers: .command)
                        .accessibilityIdentifier("expanded-composer-send")
                }
            }
            .task { editorFocused = true }
        }
        .presentationDetents([.large])
        .presentationDragIndicator(.visible)
    }
}
