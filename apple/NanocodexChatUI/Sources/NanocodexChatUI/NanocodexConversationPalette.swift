import SwiftUI
import NanocodexUI
#if os(macOS)
import AppKit
#else
import UIKit
#endif

/// Conversation colors, matching the mobile app's Ink palette.
public enum NanocodexConversationPalette {
    public static let surface = ChatPalette.userBubble
    public static let text = Color.primary
    public static let muted = Color.secondary
    #if os(macOS)
    public static let background = Color(nsColor: .textBackgroundColor)
    public static let running = Color(nsColor: NSColor(name: nil) { appearance in
        appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
            ? NSColor(srgbRed: 0.35, green: 0.85, blue: 0.5, alpha: 1)
            : NSColor(srgbRed: 0.17, green: 0.45, blue: 0.24, alpha: 1)
    })
    public static let userMessage = Color(nsColor: NSColor(name: nil) { appearance in
        appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
            ? NSColor(srgbRed: 0.08, green: 0.25, blue: 0.47, alpha: 1)
            : NSColor(srgbRed: 0.84, green: 0.92, blue: 1, alpha: 1)
    })
    #else
    public static let background = Color(uiColor: .systemBackground)
    public static let running = Color(uiColor: UIColor { traits in
        traits.userInterfaceStyle == .dark ? UIColor(red: 0.35, green: 0.85, blue: 0.5, alpha: 1)
            : UIColor(red: 0.17, green: 0.45, blue: 0.24, alpha: 1)
    })
    public static let userMessage = Color(uiColor: UIColor { traits in
        traits.userInterfaceStyle == .dark ? UIColor(red: 0.08, green: 0.25, blue: 0.47, alpha: 1)
            : UIColor(red: 0.84, green: 0.92, blue: 1, alpha: 1)
    })
    #endif
}
