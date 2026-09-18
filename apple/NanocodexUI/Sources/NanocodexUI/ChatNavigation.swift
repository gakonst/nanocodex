import SwiftUI

/// The native app's header material, shared with embedded conversations.
public struct ChatHeaderGlass: ViewModifier {
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency
    public init() {}
    public func body(content: Content) -> some View {
        #if os(iOS)
        if reduceTransparency {
            content.background(Color(uiColor: .secondarySystemGroupedBackground), in: RoundedRectangle(cornerRadius: 24))
        } else if #available(iOS 26.0, *) {
            content.glassEffect(.regular, in: RoundedRectangle(cornerRadius: 24))
        } else {
            content.background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 24))
        }
        #else
        content.background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 24))
        #endif
    }
}

public struct ChatConversationHeader<Leading: View, Title: View, Trailing: View>: View {
    private let leading: Leading
    private let title: Title
    private let trailing: Trailing
    public init(@ViewBuilder leading: () -> Leading, @ViewBuilder title: () -> Title,
                @ViewBuilder trailing: () -> Trailing) {
        self.leading = leading(); self.title = title(); self.trailing = trailing()
    }
    public var body: some View {
        HStack(spacing: 12) { leading; title; trailing }
            .buttonStyle(.plain).font(.system(size: 18, weight: .medium))
            .padding(.horizontal, 16).padding(.vertical, 6)
    }
}

/// Shared child-conversation row. Navigation and membership remain host-owned.
public struct ChatConversationRowLabel: View {
    public let title: String
    public let running: Bool
    public let selected: Bool
    public let runningColor: Color
    public init(title: String, running: Bool, selected: Bool, runningColor: Color) {
        self.title = title; self.running = running; self.selected = selected; self.runningColor = runningColor
    }
    public var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "bubble.left").font(.caption).foregroundStyle(.secondary)
            Text(title).font(.subheadline).lineLimit(2)
            Spacer(minLength: 4)
            if running { Circle().fill(runningColor).frame(width: 6, height: 6) }
        }
        .padding(.leading, 28).padding(.trailing, 14).padding(.vertical, 12)
        .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
        .background(selected ? ChatPalette.userBubble : Color.clear, in: RoundedRectangle(cornerRadius: 12))
        .contentShape(Rectangle())
    }
}
