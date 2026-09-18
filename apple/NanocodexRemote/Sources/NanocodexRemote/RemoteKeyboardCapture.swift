#if os(macOS)
import AppKit
import ApplicationServices

public enum RemoteKeyboardPermission {
    public static var isGranted: Bool { AXIsProcessTrusted() }
    /// Call only from an explicit user action such as an Allow System Shortcuts button.
    @discardableResult public static func request() -> Bool {
        AXIsProcessTrustedWithOptions([kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true] as CFDictionary)
    }
}

/// Installed only while the controlling canvas is first responder. Accessibility
/// permission is needed to intercept system shortcuts such as Command-Tab.
final class RemoteKeyboardCapture {
    var isActive: () -> Bool = { false }
    var handle: (NSEvent) -> Void = { _ in }
    var release: () -> Void = {}
    var statusChanged: () -> Void = {}
    private var lastAttempt = Date.distantPast
    var capturesSystemShortcuts: Bool { tap.map { CGEvent.tapIsEnabled(tap: $0) } ?? false }
    private var tap: CFMachPort?
    private var source: CFRunLoopSource?
    private var monitor: Any?
    private var observers: [NSObjectProtocol] = []

    func start() {
        if monitor != nil {
            guard tap == nil, AXIsProcessTrusted(), Date().timeIntervalSince(lastAttempt) >= 2 else { return }
            stop()
        }
        lastAttempt = Date()
        let mask = [CGEventType.keyDown, .keyUp, .flagsChanged].reduce(CGEventMask(0)) { $0 | (1 << $1.rawValue) }
        if AXIsProcessTrusted() {
            tap = CGEvent.tapCreate(tap: .cgSessionEventTap, place: .headInsertEventTap,
                options: .defaultTap, eventsOfInterest: mask, callback: { _, type, event, context in
                    guard let context else { return Unmanaged.passUnretained(event) }
                    let capture = Unmanaged<RemoteKeyboardCapture>.fromOpaque(context).takeUnretainedValue()
                    if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
                        capture.release()
                        if capture.isActive(), let tap = capture.tap { CGEvent.tapEnable(tap: tap, enable: true) }
                        return Unmanaged.passUnretained(event)
                    }
                    guard capture.isActive(), let key = NSEvent(cgEvent: event) else { return Unmanaged.passUnretained(event) }
                    capture.handle(key)
                    return nil
                }, userInfo: Unmanaged.passUnretained(self).toOpaque())
            if let tap {
                source = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, tap, 0)
                CFRunLoopAddSource(CFRunLoopGetMain(), source, .commonModes)
            }
        }
        // Also covers ordinary app shortcuts when Accessibility is unavailable.
        monitor = NSEvent.addLocalMonitorForEvents(matching: [.keyDown, .keyUp, .flagsChanged]) { [weak self] event in
            guard let self, self.isActive() else { return event }
            self.handle(event)
            return nil
        }
        observers.append(NotificationCenter.default.addObserver(forName: NSApplication.didBecomeActiveNotification, object: nil, queue: .main) { [weak self] _ in
            guard let self, self.isActive() else { return }
            self.start(); self.statusChanged()
        })
        for name in [NSApplication.didResignActiveNotification, NSWindow.didResignKeyNotification] {
            observers.append(NotificationCenter.default.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                guard let self, !self.isActive() else { return }
                self.release()
            })
        }
    }
    func stop() {
        if let monitor { NSEvent.removeMonitor(monitor) }; monitor = nil
        if let tap { CGEvent.tapEnable(tap: tap, enable: false); CFMachPortInvalidate(tap) }
        if let source { CFRunLoopRemoveSource(CFRunLoopGetMain(), source, .commonModes) }
        source = nil; tap = nil
        observers.forEach(NotificationCenter.default.removeObserver); observers.removeAll()
    }
    deinit { stop() }
}
#endif
