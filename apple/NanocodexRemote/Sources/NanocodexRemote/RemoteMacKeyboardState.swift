#if os(macOS)
import CoreGraphics

/// Quartz uses flagsChanged plus alphaShift for Caps Lock. Keep its locking
/// state separate from momentary keys so lease cleanup cannot toggle it off.
struct RemoteMacKeyboardState {
    private(set) var keys = Set<CGKeyCode>()
    private(set) var capsLock: Bool
    init(capsLock: Bool) { self.capsLock = capsLock }
    mutating func apply(key: CGKeyCode, down: Bool) -> CGEventType? {
        let repeated = keys.contains(key)
        if down { keys.insert(key) } else { keys.remove(key) }
        if key == 57 {
            guard down, !repeated else { return nil }
            capsLock.toggle()
            return .flagsChanged
        }
        return down ? .keyDown : .keyUp
    }
    mutating func release() -> [CGKeyCode] {
        let released = keys.filter { $0 != 57 }.sorted()
        keys.removeAll()
        return released
    }
    var flags: CGEventFlags {
        var flags: CGEventFlags = capsLock ? .maskAlphaShift : []
        if !keys.isDisjoint(with: [55, 54]) { flags.insert(.maskCommand) }
        if !keys.isDisjoint(with: [56, 60]) { flags.insert(.maskShift) }
        if !keys.isDisjoint(with: [58, 61]) { flags.insert(.maskAlternate) }
        if !keys.isDisjoint(with: [59, 62]) { flags.insert(.maskControl) }
        return flags
    }
}
#endif
