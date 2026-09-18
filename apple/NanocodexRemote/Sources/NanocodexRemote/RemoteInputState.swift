import Foundation

/// Focus is an input boundary, not an exit from the immersive UI.
struct RemoteDashboardFocus: Equatable {
    var immersive: Bool
    var active: Bool
    var connected: Bool
    var selection: String? = nil
}

struct RemoteDashboardControlPolicy {
    enum Action: Equatable { case none, acquire, release }
    private(set) var previous = RemoteDashboardFocus(immersive: false, active: true, connected: false)
    private(set) var wantsControl = false
    private var blocked = false
    var allowsAcquisition: Bool { !previous.immersive || previous.active }
    mutating func requestControl() { wantsControl = true; blocked = false }
    mutating func clearIntent() { wantsControl = false; blocked = true }
    mutating func update(_ next: RemoteDashboardFocus) -> Action {
        let old = previous; previous = next
        if old.immersive && !next.immersive {
            wantsControl = false
            return .release
        }
        let entered = next.immersive && !old.immersive
        let changedSelection = old.selection != next.selection
        if entered { wantsControl = next.active; blocked = false }
        if changedSelection {
            // A first selection can finish a foreground request in the
            // background; a different viewer cannot inherit that request.
            if old.selection != nil || next.active {
                wantsControl = !blocked && next.selection != nil && next.immersive && next.active
            }
        }
        if old.active && !next.active { return .release }
        if next.immersive && next.active && next.connected && wantsControl &&
            (entered || changedSelection || !old.active || !old.connected) { return .acquire }
        return .none
    }
}

#if os(macOS)
import AppKit

struct RemoteKeyTransition: Equatable {
    let key: UInt16
    let down: Bool
}

/// Device-dependent masks from IOKit/hidsystem/IOLLEvent.h distinguish both
/// sides even when one modifier is released while its counterpart stays down.
struct RemoteModifierState {
    private(set) var held = Set<UInt16>()
    private static let groups: [(NSEvent.ModifierFlags, UInt16, UInt, UInt16, UInt)] = [
        (.control, 224, 0x1, 228, 0x2000), (.shift, 225, 0x2, 229, 0x4),
        (.option, 226, 0x20, 230, 0x40), (.command, 227, 0x8, 231, 0x10),
    ]
    mutating func reset() { held.removeAll() }
    mutating func reconcile(_ flags: NSEvent.ModifierFlags, changedKey: UInt16? = nil) -> [RemoteKeyTransition] {
        var next = Set<UInt16>()
        for (flag, left, leftMask, right, rightMask) in Self.groups where flags.contains(flag) {
            if flags.rawValue & (leftMask | rightMask) != 0 {
                if flags.rawValue & leftMask != 0 { next.insert(left) }
                if flags.rawValue & rightMask != 0 { next.insert(right) }
            } else {
                // AppKit/synthetic events can omit side bits. Preserve known
                // sides and use the changing key when available, else left.
                var sides = held.intersection([left, right])
                if let changedKey, changedKey == left || changedKey == right {
                    if sides.contains(changedKey) { sides.remove(changedKey) } else { sides.insert(changedKey) }
                } else if sides.isEmpty { sides.insert(left) }
                next.formUnion(sides)
            }
        }
        let changes = held.subtracting(next).sorted().map { RemoteKeyTransition(key: $0, down: false) }
            + next.subtracting(held).sorted().map { RemoteKeyTransition(key: $0, down: true) }
        held = next
        return changes
    }
    static func isExit(keyCode: UInt16, flags: NSEvent.ModifierFlags) -> Bool {
        keyCode == 53 && flags.intersection([.command, .shift, .control, .option, .function]) == [.command, .shift]
    }
}

/// Caps Lock is a locking key, not a modifier held until releaseAll. The wire's
/// existing HID 57 key press toggles the remote lock once per physical change.
struct RemoteCapsLockState {
    private var observed: Bool?
    mutating func reset() { observed = nil }
    mutating func observe(_ enabled: Bool, changed: Bool) -> [RemoteKeyTransition] {
        defer { observed = enabled }
        guard changed, observed != enabled else { return [] }
        return [.init(key: 57, down: true), .init(key: 57, down: false)]
    }
}

struct RemotePointerState {
    private(set) var buttons = Set<Int>()
    var dragging: Bool { !buttons.isEmpty }
    mutating func button(_ button: Int, down: Bool) {
        if down { buttons.insert(button) } else { buttons.remove(button) }
    }
    mutating func reset() { buttons.removeAll() }
}
#endif
