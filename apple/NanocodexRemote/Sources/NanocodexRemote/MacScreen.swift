#if os(macOS)
import AppKit
import ScreenCaptureKit
import WebRTC

/// The native host owns capture and OS permission. Human viewers and managed
/// agents use the same explicitly shared display and validated input backend.
public final class MacScreen: NSObject, SCStreamOutput, SCStreamDelegate, @unchecked Sendable {
    private let source: RTCVideoSource
    private let capturer: RTCVideoCapturer
    private let frames = DispatchQueue(label: "nanocodex.remote.capture", qos: .userInteractive)
    private var stream: SCStream?
    private let lock = NSLock()
    private var stopped = true
    private var displayID: CGDirectDisplayID?
    private var displayBounds = CGRect.zero
    private let snapshotBuffer = RemoteSnapshotBuffer()
    public var onFailure: @Sendable (Error) -> Void = { _ in }

    public init(source: RTCVideoSource) {
        self.source = source; capturer = RTCVideoCapturer(delegate: source)
        super.init()
    }

    public static func surfaces() async throws -> [RemoteSurface] {
        guard CGPreflightScreenCaptureAccess() else { throw RemoteError.screenPermission }
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
        return content.displays.map { display in
            RemoteSurface(id: "display-\(display.displayID)", name: "Display \(display.displayID)", kind: .desktop,
                          width: display.width, height: display.height, controllable: CGPreflightPostEventAccess(), agentTools: true)
        }
    }

    @MainActor public static func requestScreenPermission() -> Bool { CGRequestScreenCaptureAccess() }
    @MainActor public static func requestInputPermission() -> Bool { CGRequestPostEventAccess() }

    /// Returns the native logical rectangle used for coordinate mapping.
    @MainActor public func start(surfaceID: String, maxDimension: Int = 1920, fps: Int = 60) async throws -> CGRect {
        guard stream == nil, (320...4096).contains(maxDimension), (1...60).contains(fps) else { throw RemoteError.invalidMessage }
        guard CGPreflightScreenCaptureAccess() else { throw RemoteError.screenPermission }
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
        guard let display = content.displays.first(where: { "display-\($0.displayID)" == surfaceID }) else { throw RemoteError.unavailable }
        let filter = SCContentFilter(display: display, excludingWindows: [])
        let config = SCStreamConfiguration()
        let scale = min(1, Double(maxDimension) / Double(max(display.width, display.height)))
        config.width = max(2, Int(Double(display.width) * scale) / 2 * 2)
        config.height = max(2, Int(Double(display.height) * scale) / 2 * 2)
        config.minimumFrameInterval = CMTime(value: 1, timescale: Int32(fps))
        config.queueDepth = 3
        config.pixelFormat = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
        config.showsCursor = true; config.capturesAudio = false
        config.scalesToFit = true
        let capture = SCStream(filter: filter, configuration: config, delegate: self)
        try capture.addStreamOutput(self, type: .screen, sampleHandlerQueue: frames)
        lock.withLock { stopped = false; displayID = display.displayID; displayBounds = CGDisplayBounds(display.displayID) }
        stream = capture
        do { try await capture.startCapture() }
        catch { lock.withLock { stopped = true }; stream = nil; throw error }
        return CGDisplayBounds(display.displayID)
    }

    @MainActor public func stop() async {
        lock.withLock { stopped = true }
        snapshotBuffer.clear()
        let capture = stream; stream = nil
        try? await capture?.stopCapture()
    }

    public func stream(_ stream: SCStream, didStopWithError error: Error) {
        lock.withLock { stopped = true }; onFailure(error)
    }

    func snapshot() throws -> RemoteSnapshot {
        guard CGPreflightScreenCaptureAccess(), !lock.withLock({ stopped }) else { throw RemoteError.screenPermission }
        return try snapshotBuffer.snapshot()
    }

    public func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, !lock.withLock({ stopped }), sampleBuffer.isValid,
              let attachments = CMSampleBufferGetSampleAttachmentsArray(sampleBuffer, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
              let status = attachments.first?[.status] as? Int, status == SCFrameStatus.complete.rawValue,
              let buffer = sampleBuffer.imageBuffer else { return }
        let changed = lock.withLock {
            guard let displayID, CGDisplayBounds(displayID) != displayBounds else { return false }
            stopped = true; return true
        }
        if changed { onFailure(RemoteError.geometryChanged); return }
        snapshotBuffer.update(buffer)
        // Keep the IOSurface-backed pixel buffer; no JPEG, base64, subprocess, or
        // unbounded DispatchQueue hop exists between ScreenCaptureKit and WebRTC.
        let time = CMTimeConvertScale(sampleBuffer.presentationTimeStamp, timescale: 1_000_000_000, method: .default).value
        source.capturer(capturer, didCapture: RTCVideoFrame(buffer: RTCCVPixelBuffer(pixelBuffer: buffer), rotation: ._0, timeStampNs: time))
    }
}

@MainActor
public final class MacInput {
    private var bounds: CGRect
    private var keys = Set<CGKeyCode>()
    private var buttons = Set<Int>()
    private var position: CGPoint
    private var lastClick: (button: Int, point: CGPoint, time: TimeInterval, count: Int64)?
    private var clickCounts: [Int: Int64] = [:]
    private let source = CGEventSource(stateID: .hidSystemState)
    private let displayID: CGDirectDisplayID?

    public init(bounds: CGRect, displayID: CGDirectDisplayID? = nil) throws {
        guard bounds.width > 0, bounds.height > 0, !bounds.isInfinite else { throw RemoteError.invalidMessage }
        self.bounds = bounds; self.displayID = displayID; position = CGPoint(x: bounds.midX, y: bounds.midY)
    }

    public func apply(_ event: RemoteInput) throws {
        try event.validate()
        guard CGPreflightPostEventAccess() else { throw RemoteError.inputPermission }
        if let displayID, CGDisplayBounds(displayID) != bounds { releaseAll(); throw RemoteError.geometryChanged }
        if let x = event.x, let y = event.y {
            position = CGPoint(x: bounds.minX + x * max(0, bounds.width - 1), y: bounds.minY + y * max(0, bounds.height - 1))
        }
        switch event.kind {
        case .move:
            let held = buttons.sorted().first
            mouse(type: held == 0 ? .leftMouseDragged : held == 1 ? .rightMouseDragged : held == 2 ? .otherMouseDragged : .mouseMoved,
                  button: held ?? 0)
        case .button:
            let button = event.button!, down = event.down!
            if down {
                let now = ProcessInfo.processInfo.systemUptime
                let count: Int64
                if let previous = lastClick, previous.button == button, now - previous.time <= NSEvent.doubleClickInterval,
                   hypot(position.x - previous.point.x, position.y - previous.point.y) <= 4 {
                    count = min(3, previous.count + 1)
                } else { count = 1 }
                lastClick = (button, position, now, count); clickCounts[button] = count
            }
            if down { buttons.insert(button) } else { buttons.remove(button) }
            let type: CGEventType = button == 0 ? (down ? .leftMouseDown : .leftMouseUp)
                : button == 1 ? (down ? .rightMouseDown : .rightMouseUp) : (down ? .otherMouseDown : .otherMouseUp)
            mouse(type: type, button: button)
        case .scroll:
            mouse(type: .mouseMoved, button: 0)
            let scroll = CGEvent(scrollWheelEvent2Source: source, units: .pixel, wheelCount: 2,
                                 wheel1: Int32(event.deltaY!.rounded()), wheel2: Int32(event.deltaX!.rounded()), wheel3: 0)
            scroll?.location = position; scroll?.flags = flags; scroll?.post(tap: .cghidEventTap)
        case .key:
            guard let key = RemoteKey.hidToMac[event.key!] else { throw RemoteError.invalidMessage }
            let down = event.down!
            if down { keys.insert(key) } else { keys.remove(key) }
            let keyEvent = CGEvent(keyboardEventSource: source, virtualKey: key, keyDown: down)
            keyEvent?.flags = flags; keyEvent?.post(tap: .cghidEventTap)
        case .text:
            // Quartz limits a Unicode keyboard event to 20 UTF-16 units.
            // Keep scalar boundaries intact when sending pasted/composed text.
            var units: [UniChar] = []
            func flush() {
                guard !units.isEmpty else { return }
                for down in [true, false] {
                    let keyEvent = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: down)
                    units.withUnsafeBufferPointer { keyEvent?.keyboardSetUnicodeString(stringLength: $0.count, unicodeString: $0.baseAddress!) }
                    keyEvent?.post(tap: .cghidEventTap)
                }
                units.removeAll(keepingCapacity: true)
            }
            for scalar in event.text!.unicodeScalars {
                let next = Array(String(scalar).utf16)
                if units.count + next.count > 20 { flush() }
                units += next
            }
            flush()
        case .releaseAll: releaseAll()
        }
    }

    public func releaseAll() {
        let pressedKeys = keys; keys.removeAll()
        for key in pressedKeys {
            let event = CGEvent(keyboardEventSource: source, virtualKey: key, keyDown: false)
            event?.flags = []; event?.post(tap: .cghidEventTap)
        }
        let pressedButtons = buttons; buttons.removeAll()
        for button in pressedButtons { mouse(type: button == 0 ? .leftMouseUp : button == 1 ? .rightMouseUp : .otherMouseUp, button: button) }
        lastClick = nil; clickCounts.removeAll()
    }

    private func mouse(type: CGEventType, button: Int) {
        let event = CGEvent(mouseEventSource: source, mouseType: type, mouseCursorPosition: position,
                            mouseButton: button == 0 ? .left : button == 1 ? .right : .center)
        if [.leftMouseDown, .leftMouseUp, .rightMouseDown, .rightMouseUp, .otherMouseDown, .otherMouseUp].contains(type) {
            event?.setIntegerValueField(.mouseEventClickState, value: clickCounts[button] ?? 1)
        }
        event?.flags = flags; event?.post(tap: .cghidEventTap)
    }

    private var flags: CGEventFlags {
        var flags: CGEventFlags = []
        if !keys.isDisjoint(with: [55, 54]) { flags.insert(.maskCommand) }
        if !keys.isDisjoint(with: [56, 60]) { flags.insert(.maskShift) }
        if !keys.isDisjoint(with: [58, 61]) { flags.insert(.maskAlternate) }
        if !keys.isDisjoint(with: [59, 62]) { flags.insert(.maskControl) }
        if keys.contains(63) { flags.insert(.maskSecondaryFn) }
        return flags
    }
}
#endif
