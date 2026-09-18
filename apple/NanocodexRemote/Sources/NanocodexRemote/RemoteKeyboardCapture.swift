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

/// Kept separate from event monitors so recovery can be tested without installing
/// a tap or taking input from the user's desktop.
protocol RemoteKeyboardTap: AnyObject {
    var enabled: Bool { get }
    func enable()
    func invalidate()
}

final class RemoteKeyboardTapController {
    enum Status: String { case stopped, accessibilityRequired, creationFailed, disabled, capturing }
    private(set) var status = Status.stopped
    private(set) var trusted = false
    private var tap: RemoteKeyboardTap?
    private var lastAttempt = Date.distantPast
    private let trust: () -> Bool
    private let create: () -> RemoteKeyboardTap?
    private let now: () -> Date
    init(trust: @escaping () -> Bool, now: @escaping () -> Date = Date.init,
         create: @escaping () -> RemoteKeyboardTap?) {
        self.trust = trust; self.now = now; self.create = create
    }
    var enabled: Bool { tap?.enabled == true }
    func start() {
        trusted = trust()
        guard trusted else {
            tap?.invalidate(); tap = nil
            status = .accessibilityRequired
            // Permission may be granted before the retry interval expires.
            lastAttempt = .distantPast
            return
        }
        if let tap {
            if !tap.enabled { tap.enable() }
            if tap.enabled { status = .capturing; return }
            status = .disabled
        }
        guard now().timeIntervalSince(lastAttempt) >= 2 else { return }
        lastAttempt = now()
        tap?.invalidate(); tap = nil
        guard let created = create() else { status = .creationFailed; return }
        tap = created
        if !created.enabled { created.enable() }
        status = created.enabled ? .capturing : .disabled
    }
    func stop() {
        tap?.invalidate(); tap = nil; status = .stopped; lastAttempt = .distantPast
    }
    deinit { stop() }
}

private final class RemoteSystemKeyboardTap: RemoteKeyboardTap {
    let port: CFMachPort
    let source: CFRunLoopSource
    init?(port: CFMachPort) {
        guard let source = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, port, 0) else {
            CFMachPortInvalidate(port); return nil
        }
        self.port = port; self.source = source
        CFRunLoopAddSource(CFRunLoopGetMain(), source, .commonModes)
    }
    var enabled: Bool { CGEvent.tapIsEnabled(tap: port) }
    func enable() { CGEvent.tapEnable(tap: port, enable: true) }
    func invalidate() {
        CGEvent.tapEnable(tap: port, enable: false)
        CFMachPortInvalidate(port)
        CFRunLoopRemoveSource(CFRunLoopGetMain(), source, .commonModes)
    }
}

/// Owns capture, not the remote control lease. Focus loss releases held inputs;
/// only the dashboard's explicit exit/selection lifecycle releases immersion.
final class RemoteKeyboardCapture {
    var isActive: () -> Bool = { false }
    var handle: (NSEvent) -> Void = { _ in }
    var release: () -> Void = {}
    var statusChanged: () -> Void = {}
    var focusChanged: () -> Void = {}
    private lazy var controller = RemoteKeyboardTapController(trust: { AXIsProcessTrusted() }) { [weak self] in
        self?.makeTap()
    }
    var capturesSystemShortcuts: Bool { controller.enabled }
    var diagnostic: String { "AX trusted=\(controller.trusted); keyboard tap=\(controller.status.rawValue); capture eligible=\(isActive())" }
    var unavailableReason: String {
        switch controller.status {
        case .accessibilityRequired:
            return "This Nanocodex process does not have Accessibility access. Check Nanocodex in System Settings → Privacy & Security → Accessibility."
        case .creationFailed:
            return "Accessibility access is granted, but macOS did not create the keyboard capture tap."
        case .disabled:
            return "The keyboard capture tap is disabled. Nanocodex will retry when this screen has focus."
        case .stopped:
            return "Keyboard capture is paused while this screen does not have focus."
        case .capturing:
            return ""
        }
    }
    private var retryTimer: Timer?
    private var monitor: Any?
    private var observers: [NSObjectProtocol] = []

    private func makeTap() -> RemoteKeyboardTap? {
        let mask = [CGEventType.keyDown, .keyUp, .flagsChanged].reduce(CGEventMask(0)) { $0 | (1 << $1.rawValue) }
        guard let port = CGEvent.tapCreate(tap: .cgSessionEventTap, place: .headInsertEventTap,
            options: .defaultTap, eventsOfInterest: mask, callback: { _, type, event, context in
                guard let context else { return Unmanaged.passUnretained(event) }
                let capture = Unmanaged<RemoteKeyboardCapture>.fromOpaque(context).takeUnretainedValue()
                if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
                    capture.release()
                    if capture.isActive() { capture.controller.start() } else { capture.controller.stop() }
                    capture.statusChanged()
                    return Unmanaged.passUnretained(event)
                }
                guard capture.isActive(), let key = NSEvent(cgEvent: event) else { return Unmanaged.passUnretained(event) }
                capture.handle(key)
                return nil
            }, userInfo: Unmanaged.passUnretained(self).toOpaque()) else { return nil }
        return RemoteSystemKeyboardTap(port: port)
    }
    func start() {
        controller.start()
        guard monitor == nil else { statusChanged(); return }
        // Ordinary app shortcuts still work when system capture is unavailable.
        monitor = NSEvent.addLocalMonitorForEvents(matching: [.keyDown, .keyUp, .flagsChanged]) { [weak self] event in
            guard let self, self.isActive() else { return event }
            self.handle(event)
            return nil
        }
        for name in [NSApplication.didBecomeActiveNotification, NSWindow.didBecomeKeyNotification] {
            observers.append(NotificationCenter.default.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                guard let self else { return }
                self.focusChanged()
                if self.isActive() { self.controller.start() }
                self.statusChanged()
            })
        }
        for name in [NSApplication.didResignActiveNotification, NSWindow.didResignKeyNotification] {
            observers.append(NotificationCenter.default.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                guard let self, !self.isActive() else { return }
                self.release()
                self.controller.stop()
                self.statusChanged()
            })
        }
        let timer = Timer(timeInterval: 2, repeats: true) { [weak self] _ in
            guard let self, self.isActive(), !self.controller.enabled else { return }
            self.controller.start(); self.statusChanged()
        }
        timer.tolerance = 0.25
        RunLoop.main.add(timer, forMode: .common)
        retryTimer = timer
        statusChanged()
    }
    func pause() { controller.stop(); statusChanged() }
    func stop() {
        retryTimer?.invalidate(); retryTimer = nil
        if let monitor { NSEvent.removeMonitor(monitor) }; monitor = nil
        controller.stop()
        observers.forEach(NotificationCenter.default.removeObserver); observers.removeAll()
    }
    deinit { stop() }
}
#endif
