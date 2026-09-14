import SwiftUI
#if os(macOS)
import AppKit
#else
import UIKit
#endif

public enum ChatPalette {
    public static let background = adaptive(light: 0xffffff, dark: 0x212121)
    public static let sidebar = adaptive(light: 0xf9f9f9, dark: 0x191919)
    public static let composer = adaptive(light: 0xffffff, dark: 0x303030)
    public static let userBubble = adaptive(light: 0xf4f4f4, dark: 0x303030)

    private static func adaptive(light: Int, dark: Int) -> Color {
        #if os(macOS)
        return Color(nsColor: NSColor(name: nil) { appearance in
            let value = appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua ? dark : light
            return NSColor(srgbRed: Double((value >> 16) & 255) / 255, green: Double((value >> 8) & 255) / 255, blue: Double(value & 255) / 255, alpha: 1)
        })
        #else
        return Color(uiColor: UIColor { traits in
            let value = traits.userInterfaceStyle == .dark ? dark : light
            return UIColor(red: Double((value >> 16) & 255) / 255, green: Double((value >> 8) & 255) / 255, blue: Double(value & 255) / 255, alpha: 1)
        })
        #endif
    }
}

public struct ChatCopyButton: View {
    private let text: String
    private let label: String
    private let showsLabel: Bool
    @State private var copied = false
    @State private var hovered = false

    public init(text: String, label: String = "Copy response", showsLabel: Bool = false) {
        self.text = text
        self.label = label
        self.showsLabel = showsLabel
    }

    public var body: some View {
        Button {
            #if os(macOS)
            NSPasteboard.general.clearContents()
            copied = NSPasteboard.general.setString(text, forType: .string)
            #else
            UIPasteboard.general.string = text
            copied = true
            #endif
        } label: {
            HStack(spacing: 6) {
                Image(systemName: copied ? "checkmark" : "doc.on.doc")
                if showsLabel || copied { Text(copied ? "Copied" : "Copy") }
            }
            .font(.system(size: 13))
            .foregroundStyle(.secondary)
            .padding(.horizontal, 8)
            #if os(iOS)
            .frame(minHeight: 44)
            #else
            .frame(minHeight: 32)
            #endif
            .background(hovered ? Color.primary.opacity(0.06) : .clear, in: RoundedRectangle(cornerRadius: 8))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hovered = $0 }
        .help(copied ? "Copied" : label)
        .accessibilityLabel(copied ? "Copied" : label)
        .accessibilityIdentifier(label == "Copy code" ? "copy-code" : "copy-response")
        .task(id: copied) {
            guard copied else { return }
            do { try await Task.sleep(for: .seconds(2)); copied = false } catch {}
        }
    }
}
