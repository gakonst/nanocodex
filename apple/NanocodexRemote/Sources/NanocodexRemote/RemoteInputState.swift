import Foundation

/// Immersion survives app/window focus changes. A late connection may acquire
/// on the next activation, but a host revocation never causes a blind retake.
struct RemoteDashboardFocus: Equatable {
    var immersive: Bool
    var active: Bool
    var connected: Bool
}

struct RemoteDashboardControlPolicy {
    enum Action: Equatable { case none, acquire, release }
    private var previous = RemoteDashboardFocus(immersive: false, active: true, connected: false)
    private var pendingAcquire = false
    mutating func update(_ next: RemoteDashboardFocus) -> Action {
        let old = previous; previous = next
        if old.immersive && !next.immersive {
            pendingAcquire = false
            return .release
        }
        if next.immersive && (!old.immersive || (!old.connected && next.connected)) { pendingAcquire = true }
        if pendingAcquire && next.immersive && next.active && next.connected {
            pendingAcquire = false
            return .acquire
        }
        if old.active && !next.active && !next.immersive { return .release }
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
        keyCode == 53 && flags.intersection([.command, .shift, .control, .option]) == [.command, .shift]
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
