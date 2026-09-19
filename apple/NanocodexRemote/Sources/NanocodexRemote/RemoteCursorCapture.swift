#if os(macOS)
import AppKit

/// Balances AppKit's process-wide hide count across repeated view updates.
final class RemoteCursorCapture {
    private(set) var hidden = false
    private(set) var locked = false
    private let associate: (Bool) -> Bool
    private let hide: () -> Void
    private let unhide: () -> Void

    init(hide: @escaping () -> Void = { NSCursor.hide() },
         unhide: @escaping () -> Void = { NSCursor.unhide() },
         associate: @escaping (Bool) -> Bool = { CGAssociateMouseAndMouseCursorPosition($0 ? 1 : 0) == .success }) {
        self.hide = hide; self.unhide = unhide; self.associate = associate
    }

    func update(hidden shouldHide: Bool, locked shouldLock: Bool = false) {
        let lock = shouldHide && shouldLock
        if locked != lock, associate(!lock) { locked = lock }
        guard hidden != shouldHide else { return }
        hidden = shouldHide
        if shouldHide { hide() } else { unhide() }
    }

    deinit { if locked { _ = associate(true) }; if hidden { unhide() } }
}
#endif
