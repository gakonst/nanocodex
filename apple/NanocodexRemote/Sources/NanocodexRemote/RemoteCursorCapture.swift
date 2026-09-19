#if os(macOS)
import AppKit

/// Balances AppKit's process-wide hide count across repeated view updates.
final class RemoteCursorCapture {
    private(set) var hidden = false
    private let hide: () -> Void
    private let unhide: () -> Void

    init(hide: @escaping () -> Void = { NSCursor.hide() },
         unhide: @escaping () -> Void = { NSCursor.unhide() }) {
        self.hide = hide; self.unhide = unhide
    }

    func update(hidden shouldHide: Bool) {
        guard hidden != shouldHide else { return }
        hidden = shouldHide
        if shouldHide { hide() } else { unhide() }
    }

    deinit { if hidden { unhide() } }
}
#endif
