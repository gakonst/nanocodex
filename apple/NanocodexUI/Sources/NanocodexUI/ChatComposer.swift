import SwiftUI

/// The composer surface, including its focus ring and outer spacing.
/// Content owns pending messages, attachments, and the input row.
public struct ChatComposerShell<Content: View>: View {
    private let focused: Bool
    private let background: Color
    private let content: Content

    public init(focused: Bool, background: Color, @ViewBuilder content: () -> Content) {
        self.focused = focused
        self.background = background
        self.content = content()
    }

    public var body: some View {
        VStack(spacing: 0) { content }
            .background(ChatPalette.composer, in: RoundedRectangle(cornerRadius: 28))
            .overlay(RoundedRectangle(cornerRadius: 28).strokeBorder(Color.primary.opacity(focused ? 0.18 : 0.1)))
            .shadow(color: .black.opacity(0.035), radius: 8, y: 2)
            .padding(.horizontal, 12).padding(.top, 4).padding(.bottom, 6)
            .background(background)
    }
}

/// Bottom-aligned controls and editor with an independent expansion-control slot.
public struct ChatComposerInputRow<Content: View, Expansion: View>: View {
    private let hasPendingMessages: Bool
    private let content: Content
    private let expansion: Expansion

    public init(
        hasPendingMessages: Bool = false,
        @ViewBuilder content: () -> Content,
        @ViewBuilder expansion: () -> Expansion
    ) {
        self.hasPendingMessages = hasPendingMessages
        self.content = content()
        self.expansion = expansion()
    }

    public var body: some View {
        HStack(alignment: .bottom, spacing: 2) { content }
            .overlay(alignment: .topTrailing) { expansion }
            .padding(.horizontal, 4).padding(.bottom, 4)
            .padding(.top, hasPendingMessages ? 0 : 4)
            .accessibilityElement(children: .contain)
    }
}

/// A control label with the composer's 44-point rectangular hit target.
/// The caller owns the Button, its action, style, and accessibility semantics.
public struct ChatComposerControlLabel<Content: View>: View {
    private let content: Content

    public init(@ViewBuilder content: () -> Content) {
        self.content = content()
    }

    public var body: some View {
        content.frame(width: 44, height: 44).contentShape(Rectangle())
    }
}

/// The circular send/stop label; accepts an icon, progress indicator, or other content.
public struct ChatComposerPrimaryControlLabel<Content: View>: View {
    private let active: Bool
    private let accent: Color
    private let foreground: Color
    private let content: Content

    public init(active: Bool, accent: Color = .primary, foreground: Color, @ViewBuilder content: () -> Content) {
        self.active = active
        self.accent = accent
        self.foreground = foreground
        self.content = content()
    }

    public var body: some View {
        ChatComposerControlLabel {
            content.font(.system(size: 16, weight: .semibold))
                .frame(width: 32, height: 32)
                .background(accent.opacity(active ? 1 : 0.22), in: Circle())
                .foregroundStyle(foreground)
        }
    }
}
