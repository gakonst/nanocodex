import SwiftUI
import Combine
import WebRTC

/// The renderer and input use the same fitted rectangle. Letterbox clicks never
/// reach the host, and dragging beyond the picture clamps to its nearest edge.
private func fitted(_ surface: CGSize, in bounds: CGRect) -> CGRect {
    guard surface.width > 0, surface.height > 0 else { return bounds }
    let scale = min(bounds.width / surface.width, bounds.height / surface.height)
    let size = CGSize(width: surface.width * scale, height: surface.height * scale)
    return CGRect(x: bounds.midX - size.width / 2, y: bounds.midY - size.height / 2, width: size.width, height: size.height)
}

#if os(macOS)
import AppKit

extension Notification.Name {
    static let remoteToggleFullScreen = Notification.Name("NanocodexRemote.toggleFullScreen")
}

public struct RemoteCanvas: NSViewRepresentable {
    @ObservedObject var viewer: RemoteViewer
    public init(viewer: RemoteViewer) { self.viewer = viewer }
    public func makeNSView(context: Context) -> MacRemoteViewport { MacRemoteViewport(viewer: viewer) }
    public func updateNSView(_ view: MacRemoteViewport, context: Context) { view.update(viewer) }
    public static func dismantleNSView(_ view: MacRemoteViewport, coordinator: ()) { view.detach() }
}

public final class MacRemoteViewport: NSView, NSWindowDelegate {
    let canvas: MacRemoteCanvas
    let scrollView = NSScrollView()
    private let toolbar = NSStackView()
    let content = RemoteViewportContent()
    private var fullscreenWindow: NSWindow?
    private let controlButton = NSButton(title: "Take Control", target: nil, action: nil)
    private let captureButton = NSButton(checkboxWithTitle: "Lock Mouse", target: nil, action: nil)
    private let microphoneButton = NSButton(title: "Microphone Off", target: nil, action: nil)
    private let speakersButton = NSButton(title: "Mute Sound", target: nil, action: nil)
    private let exitButton = NSButton(title: "Exit Full Screen  ⌃⌘F", target: nil, action: nil)
    private let hint = NSTextField(labelWithString: "⌘⇧Esc releases control")
    private weak var viewer: RemoteViewer?
    private var fullscreenObserver: NSObjectProtocol?
    public override var isFlipped: Bool { true }

    init(viewer: RemoteViewer) {
        self.viewer = viewer
        canvas = MacRemoteCanvas(viewer: viewer)
        super.init(frame: .zero)
        wantsLayer = true; layer?.backgroundColor = NSColor.black.cgColor
        scrollView.drawsBackground = true; scrollView.backgroundColor = .black
        scrollView.allowsMagnification = true; scrollView.minMagnification = 1; scrollView.maxMagnification = 6
        scrollView.hasHorizontalScroller = true; scrollView.hasVerticalScroller = true
        scrollView.autohidesScrollers = true
        scrollView.documentView = canvas
        content.scrollView = scrollView
        content.toolbar = toolbar
        content.autoresizingMask = [.width, .height]
        addSubview(content)
        content.addSubview(scrollView)
        toolbar.orientation = .horizontal; toolbar.spacing = 16; toolbar.edgeInsets = NSEdgeInsets(top: 8, left: 16, bottom: 8, right: 16)
        toolbar.wantsLayer = true; toolbar.layer?.backgroundColor = NSColor.windowBackgroundColor.cgColor
        controlButton.target = self; controlButton.action = #selector(toggleControl); controlButton.bezelStyle = .rounded
        captureButton.target = self; captureButton.action = #selector(toggleCapture)
        captureButton.toolTip = "For games: click the screen to lock the mouse. ⌘⇧Esc releases control."
        captureButton.setAccessibilityIdentifier("remote-fullscreen-lock-mouse")
        exitButton.target = self; exitButton.action = #selector(toggleRemoteFullScreen); exitButton.bezelStyle = .rounded
        exitButton.setAccessibilityIdentifier("remote-exit-fullscreen")
        exitButton.keyEquivalent = "f"; exitButton.keyEquivalentModifierMask = [.control, .command]
        microphoneButton.target = self; microphoneButton.action = #selector(toggleMicrophone); microphoneButton.bezelStyle = .rounded
        microphoneButton.setAccessibilityIdentifier("remote-fullscreen-microphone")
        speakersButton.target = self; speakersButton.action = #selector(toggleSpeakers); speakersButton.bezelStyle = .rounded
        speakersButton.setAccessibilityIdentifier("remote-fullscreen-speakers")
        hint.font = .systemFont(ofSize: 12); hint.textColor = .secondaryLabelColor
        let spacer = NSView(); spacer.setContentHuggingPriority(.defaultLow, for: .horizontal)
        for view in [controlButton, captureButton, hint, spacer, speakersButton, microphoneButton, exitButton] { toolbar.addArrangedSubview(view) }
        content.addSubview(toolbar); toolbar.isHidden = true
        canvas.toggleFullScreen = { [weak self] in self?.toggleRemoteFullScreen() }
        fullscreenObserver = NotificationCenter.default.addObserver(forName: .remoteToggleFullScreen, object: viewer, queue: .main) { [weak self] _ in
            self?.toggleRemoteFullScreen()
        }
        update(viewer)
    }
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
    func update(_ viewer: RemoteViewer) {
        self.viewer = viewer
        canvas.update(viewer)
        controlButton.title = viewer.controlling ? "Release Control" : "Take Control"
        controlButton.keyEquivalent = viewer.controlling ? "\u{1b}" : ""
        controlButton.keyEquivalentModifierMask = [.command, .shift]
        controlButton.setAccessibilityIdentifier(viewer.controlling ? "remote-fullscreen-release-control" : "remote-fullscreen-take-control")
        controlButton.isEnabled = viewer.connected && viewer.hand?.controllable == true
        captureButton.isHidden = !viewer.controlling || !viewer.relativePointer
        captureButton.state = viewer.captureMouse ? .on : .off
        microphoneButton.isHidden = !viewer.supportsMicrophone
        microphoneButton.title = viewer.microphonePending ? "Cancel Microphone" : viewer.microphoneEnabled ? "Mute Microphone" : "Enable Microphone"
        microphoneButton.isEnabled = viewer.controlling && viewer.connected
        microphoneButton.toolTip = viewer.microphoneError ?? viewer.microphoneSetupHint
        speakersButton.title = viewer.speakersEnabled ? "Mute Sound" : "Enable Sound"
        speakersButton.isEnabled = viewer.connected && viewer.supportsSpeakers
        hint.stringValue = viewer.controlling ? (viewer.captureMouse ? "Click screen to lock · ⌘⇧Esc releases" : "⌘⇧Esc releases control") : "View only"
    }
    @objc private func toggleMicrophone() {
        guard let viewer else { return }
        viewer.setMicrophoneEnabled(!viewer.microphoneEnabled && !viewer.microphonePending)
    }
    @objc private func toggleSpeakers() {
        guard let viewer else { return }
        viewer.setSpeakersEnabled(!viewer.speakersEnabled)
    }
    @objc private func toggleControl() {
        guard let viewer else { return }
        if viewer.controlling { canvas.releaseHeldInput(); viewer.releaseControl() } else { viewer.takeControl() }
    }
    @objc private func toggleCapture() {
        canvas.releaseHeldInput()
        viewer?.captureMouse = captureButton.state == .on
    }
    @objc func toggleRemoteFullScreen() {
        canvas.releaseHeldInput()
        if let fullscreenWindow {
            if fullscreenWindow.styleMask.contains(.fullScreen) { fullscreenWindow.toggleFullScreen(nil) }
            else { restoreInline() }
            return
        }
        guard let screen = window?.screen else { return }
        let full = NSWindow(contentRect: screen.visibleFrame, styleMask: [.titled, .closable, .resizable, .miniaturizable], backing: .buffered, defer: false)
        full.isReleasedWhenClosed = false
        full.title = viewer?.hand?.name ?? "Remote Screen"
        full.collectionBehavior = [.fullScreenPrimary]
        full.delegate = self
        fullscreenWindow = full
        content.removeFromSuperview()
        full.contentView = content
        content.fullscreen = true; canvas.fullscreen = true; toolbar.isHidden = false
        content.needsLayout = true
        full.makeKeyAndOrderFront(nil)
        full.makeFirstResponder(canvas)
        full.toggleFullScreen(nil)
    }
    private func restoreInline() {
        guard let full = fullscreenWindow else { return }
        canvas.releaseHeldInput()
        fullscreenWindow = nil; full.delegate = nil
        full.contentView = nil
        content.removeFromSuperview()
        addSubview(content)
        content.fullscreen = false; canvas.fullscreen = false; toolbar.isHidden = true
        content.frame = bounds; content.needsLayout = true
        full.close()
        window?.makeKeyAndOrderFront(nil)
        window?.makeFirstResponder(canvas)
    }
    public func windowWillExitFullScreen(_ notification: Notification) { canvas.releaseHeldInput() }
    public func windowDidExitFullScreen(_ notification: Notification) { restoreInline() }
    public func windowWillClose(_ notification: Notification) { restoreInline() }
    public func windowDidFailToEnterFullScreen(_ window: NSWindow) { restoreInline() }
    func detach() {
        if let fullscreenObserver { NotificationCenter.default.removeObserver(fullscreenObserver) }
        fullscreenObserver = nil
        // Detach during SwiftUI teardown must not publish viewer state.
        canvas.detach()
        if let full = fullscreenWindow { full.delegate = nil; full.close(); fullscreenWindow = nil }
    }
    public override func resizeSubviews(withOldSize oldSize: NSSize) {
        super.resizeSubviews(withOldSize: oldSize)
        if content.superview === self { content.frame = bounds }
    }
    public override func layout() {
        super.layout()
        if content.superview === self { content.frame = bounds }
    }
}

/// The native window owns this view's size in fullscreen. SwiftUI continues to
/// own only the empty inline viewport, so its pane constraints cannot shrink it.
final class RemoteViewportContent: NSView {
    weak var scrollView: NSScrollView?
    weak var toolbar: NSStackView?
    var fullscreen = false { didSet { sizeContents() } }
    override var isFlipped: Bool { true }
    override func resizeSubviews(withOldSize oldSize: NSSize) {
        super.resizeSubviews(withOldSize: oldSize)
        sizeContents()
    }
    override func layout() { super.layout(); sizeContents() }
    private func sizeContents() {
        let height: CGFloat = fullscreen ? 48 : 0
        toolbar?.frame = CGRect(x: 0, y: 0, width: bounds.width, height: height)
        scrollView?.frame = CGRect(x: 0, y: height, width: bounds.width, height: max(0, bounds.height - height))
        if let scrollView, let canvas = scrollView.documentView, canvas.frame.size != scrollView.contentSize {
            canvas.setFrameSize(scrollView.contentSize)
        }
    }
}

public final class MacRemoteCanvas: NSView, NSTextInputClient {
    private let video = RTCMTLNSVideoView()
    private let snapshot = NSImageView()
    private weak var viewer: RemoteViewer?
    private var frameSubscription: AnyCancellable?
    private var displayedFrame: CGImage?
    private var track: RTCVideoTrack?
    private var surface = CGSize(width: 16, height: 9)
    private var pressed = Set<UInt16>()
    private var marked = NSAttributedString(string: "")
    private var pressedButtons = Set<Int>()
    private var dragging: Bool { !pressedButtons.isEmpty }
    private var tracking: NSTrackingArea?
    var fullscreen = false
    var toggleFullScreen: (() -> Void)?
    private var pointerCaptured = false
    private var focusObservers: [NSObjectProtocol] = []
    private var eventMonitor: Any?
    // Tests can exercise event routing without activating an app or capturing
    // the workstation's cursor. Production requires both window and app focus.
    var hasInputFocus: () -> Bool = { NSApp.isActive }
    private var inputEligible: Bool {
        viewer?.controlling == true && viewer?.connected == true &&
            window?.isKeyWindow == true && hasInputFocus()
    }
    // Injectable so event-dispatch tests never capture the developer's mouse.
    var capturePointer: () -> Bool = {
        guard CGAssociateMouseAndMouseCursorPosition(0) == .success else { return false }
        NSCursor.hide()
        return true
    }
    var restorePointer: () -> Void = {
        CGAssociateMouseAndMouseCursorPosition(1)
        NSCursor.unhide()
    }
    public override var isFlipped: Bool { true }
    public override var acceptsFirstResponder: Bool { viewer?.controlling == true || fullscreen }
    public override func acceptsFirstMouse(for event: NSEvent?) -> Bool { viewer?.controlling == true }
    init(viewer: RemoteViewer) {
        self.viewer = viewer
        super.init(frame: .zero)
        wantsLayer = true; layer?.backgroundColor = NSColor.black.cgColor
        setAccessibilityElement(true)
        setAccessibilityRole(.image)
        setAccessibilityLabel("Remote screen")
        setAccessibilityIdentifier("remote-canvas")
        snapshot.imageScaling = .scaleProportionallyUpOrDown
        addSubview(video); addSubview(snapshot); update(viewer)
    }
    deinit {
        MainActor.assumeIsolated {
            if let eventMonitor { NSEvent.removeMonitor(eventMonitor) }
            focusObservers.forEach(NotificationCenter.default.removeObserver)
            stopPointerCapture()
        }
    }
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
    func update(_ viewer: RemoteViewer) {
        let rebind = frameSubscription == nil || self.viewer !== viewer
        self.viewer = viewer
        if rebind {
            frameSubscription = viewer.$frame.sink { [weak self] image in self?.displayFrame(image) }
        }
        if let hand = viewer.hand { surface = CGSize(width: hand.width, height: hand.height) }
        if track !== viewer.track { track?.remove(video); track = viewer.track; track?.add(video) }
        video.isHidden = !viewer.connected || track == nil
        displayFrame(viewer.frame)
        if pointerCaptured && !viewer.captureMouse { releaseHeldInput() }
        if !viewer.controlling || !viewer.connected {
            stopPointerCapture()
            pressed.removeAll(); pressedButtons.removeAll(); unmarkText()
            if window?.firstResponder === self, !fullscreen { window?.makeFirstResponder(nil) }
        }
        needsLayout = true
    }
    private func displayFrame(_ image: CGImage?) {
        if displayedFrame !== image {
            displayedFrame = image
            snapshot.image = image.map { NSImage(cgImage: $0, size: .zero) }
        }
        snapshot.isHidden = viewer?.connected != true || image == nil
    }
    private func startPointerCapture() {
        guard !pointerCaptured, inputEligible, window?.firstResponder === self, viewer?.relativePointer == true, viewer?.captureMouse == true else { return }
        pointerCaptured = capturePointer()
    }
    public override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        releaseHeldInput()
        if let eventMonitor { NSEvent.removeMonitor(eventMonitor); self.eventMonitor = nil }
        focusObservers.forEach(NotificationCenter.default.removeObserver)
        focusObservers.removeAll()
        guard let window else { releaseHeldInput(); return }
        window.acceptsMouseMovedEvents = true
        eventMonitor = NSEvent.addLocalMonitorForEvents(matching: [.keyDown, .keyUp, .flagsChanged,
            .mouseMoved, .leftMouseDragged, .rightMouseDragged, .otherMouseDragged,
            .leftMouseDown, .leftMouseUp, .rightMouseDown, .rightMouseUp,
            .otherMouseDown, .otherMouseUp, .scrollWheel]) { [weak self] event in
            guard let self else { return event }; return self.handleLocalEvent(event)
        }
        for (name, object) in [(NSWindow.didResignKeyNotification, window as AnyObject),
                               (NSApplication.didResignActiveNotification, NSApp as AnyObject)] {
            focusObservers.append(NotificationCenter.default.addObserver(forName: name, object: object, queue: .main) { [weak self] _ in
                self?.releaseHeldInput()
            })
        }
    }
    // Runs before menu equivalents and renderer tracking. Consume each captured
    // event once; ordinary desktop clicks still use AppKit hit testing.
    func handleLocalEvent(_ event: NSEvent) -> NSEvent? {
        guard inputEligible else { releaseHeldInput(); return event }
        guard event.window === window, window?.firstResponder === self else { return event }
        if event.type == .keyDown, isSystemShortcut(event) {
            releaseHeldInput(); return event
        }
        switch event.type {
        case .keyDown: keyDown(with: event)
        case .keyUp: keyUp(with: event)
        case .flagsChanged: flagsChanged(with: event)
        default:
            guard pointerCaptured else { return event }
            switch event.type {
            case .mouseMoved, .leftMouseDragged, .rightMouseDragged, .otherMouseDragged: mouseMoved(with: event)
            case .leftMouseDown: button(event, down: true, button: 0)
            case .leftMouseUp: button(event, down: false, button: 0)
            case .rightMouseDown: button(event, down: true, button: 1)
            case .rightMouseUp: button(event, down: false, button: 1)
            case .otherMouseDown, .otherMouseUp:
                guard event.buttonNumber == 2 else { return event }
                button(event, down: event.type == .otherMouseDown, button: 2)
            case .scrollWheel: scrollWheel(with: event)
            default: return event
            }
        }
        return nil
    }
    private func stopPointerCapture() {
        if pointerCaptured { pointerCaptured = false; restorePointer() }
    }
    func releaseHeldInput() {
        if pointerCaptured || dragging || !pressed.isEmpty { viewer?.input(kind: .releaseAll) }
        stopPointerCapture(); pressed.removeAll(); pressedButtons.removeAll(); unmarkText()
    }
    public override func resizeSubviews(withOldSize oldSize: NSSize) {
        super.resizeSubviews(withOldSize: oldSize)
        sizeRenderers()
    }
    public override func layout() { super.layout(); sizeRenderers() }
    private func sizeRenderers() { video.frame = fitted(surface, in: bounds); snapshot.frame = video.frame }
    // Rendering subviews must not run AppKit mouse tracking loops: button-down
    // must reach the remote host immediately and stay down until button-up.
    public override func hitTest(_ point: NSPoint) -> NSView? {
        super.hitTest(point) == nil ? nil : self
    }
    public override func updateTrackingAreas() {
        if let tracking { removeTrackingArea(tracking) }
        let area = NSTrackingArea(rect: .zero, options: [.activeInKeyWindow, .inVisibleRect, .mouseMoved], owner: self)
        addTrackingArea(area); tracking = area; super.updateTrackingAreas()
    }
    private func point(_ event: NSEvent, clamp: Bool = false) -> CGPoint? {
        let point = convert(event.locationInWindow, from: nil), rect = video.frame
        guard rect.width > 0, rect.height > 0, clamp || rect.contains(point) else { return nil }
        return CGPoint(x: min(1, max(0, (point.x - rect.minX) / rect.width)), y: min(1, max(0, (point.y - rect.minY) / rect.height)))
    }
    private func button(_ event: NSEvent, down: Bool, button: Int) {
        guard inputEligible, down || pressedButtons.contains(button) else { return }
        let wasCaptured = pointerCaptured
        let location = wasCaptured ? nil : point(event, clamp: !down)
        guard wasCaptured || location != nil else { return }
        if down {
            window?.makeFirstResponder(self)
            syncModifiers(event.modifierFlags)
            startPointerCapture()
            pressedButtons.insert(button)
        } else { pressedButtons.remove(button) }
        // The first click positions and focuses the remote target. Keep capture
        // between clicks so later movement cannot overtake button-up on the
        // disposable motion channel or warp back to the frozen local cursor.
        viewer?.input(kind: .button, x: location.map { Double($0.x) }, y: location.map { Double($0.y) }, button: button, down: down)
    }
    public override func mouseDown(with event: NSEvent) { button(event, down: true, button: 0) }
    public override func mouseUp(with event: NSEvent) { button(event, down: false, button: 0) }
    public override func rightMouseDown(with event: NSEvent) { button(event, down: true, button: 1) }
    public override func rightMouseUp(with event: NSEvent) { button(event, down: false, button: 1) }
    public override func otherMouseDown(with event: NSEvent) { if event.buttonNumber == 2 { button(event, down: true, button: 2) } }
    public override func otherMouseUp(with event: NSEvent) { if event.buttonNumber == 2 { button(event, down: false, button: 2) } }
    public override func mouseMoved(with event: NSEvent) {
        guard inputEligible else { releaseHeldInput(); return }
        if pointerCaptured {
            let dx = min(4096, max(-4096, event.deltaX)), dy = min(4096, max(-4096, event.deltaY))
            if dx != 0 || dy != 0 { viewer?.input(kind: .relativeMove, deltaX: dx, deltaY: dy) }
            return
        }
        guard let point = point(event, clamp: dragging) else { return }
        viewer?.input(kind: .move, x: point.x, y: point.y)
    }
    public override func mouseDragged(with event: NSEvent) { mouseMoved(with: event) }
    public override func rightMouseDragged(with event: NSEvent) { mouseMoved(with: event) }
    public override func otherMouseDragged(with event: NSEvent) { mouseMoved(with: event) }
    public override func scrollWheel(with event: NSEvent) {
        guard inputEligible else { super.scrollWheel(with: event); return }
        let location = pointerCaptured ? nil : point(event)
        guard pointerCaptured || location != nil else { return }
        let scale: Double = event.hasPreciseScrollingDeltas ? 1 : 20
        viewer?.input(kind: .scroll, x: location.map { Double($0.x) }, y: location.map { Double($0.y) },
            deltaX: min(4096, max(-4096, event.scrollingDeltaX * scale)), deltaY: min(4096, max(-4096, event.scrollingDeltaY * scale)))
    }
    private func isSystemShortcut(_ event: NSEvent) -> Bool {
        event.modifierFlags.contains(.command) &&
            (event.keyCode == 48 || (event.keyCode == 53 && event.modifierFlags.contains(.option)))
    }
    private func handleLocalShortcut(_ event: NSEvent) -> Bool {
        let modifiers = event.modifierFlags.intersection([.command, .control, .option, .shift])
        if event.keyCode == 3, modifiers == [.command, .control] {
            toggleFullScreen?(); return true
        }
        if event.keyCode == 53, modifiers == [.command, .shift] {
            releaseHeldInput(); viewer?.releaseControl(); return true
        }
        if fullscreen, viewer?.controlling != true, event.keyCode == 53, modifiers.isEmpty {
            toggleFullScreen?(); return true
        }
        return false
    }
    public override func performKeyEquivalent(with event: NSEvent) -> Bool {
        guard window?.isKeyWindow == true, hasInputFocus(), window?.firstResponder === self else { return false }
        if isSystemShortcut(event) { releaseHeldInput(); return false }
        if handleLocalShortcut(event) { return true }
        guard viewer?.controlling == true else { return false }
        keyDown(with: event); return true
    }
    public override func keyDown(with event: NSEvent) {
        guard window?.isKeyWindow == true, hasInputFocus(), window?.firstResponder === self else { return }
        if isSystemShortcut(event) { releaseHeldInput(); return }
        if handleLocalShortcut(event) { return }
        guard inputEligible else { return }
        syncModifiers(event.modifierFlags)
        if viewer?.captureMouse != true, viewer?.hand?.kind != .vm,
           event.modifierFlags.intersection([.command, .control]).isEmpty,
           let characters = event.characters, characters.unicodeScalars.allSatisfy({ $0.value >= 32 && $0.value < 0xF700 }) {
            interpretKeyEvents([event])
        } else if let key = RemoteKey.macToHID[event.keyCode] {
            // Games need a held physical key, not text commits or repeated downs.
            if pressed.insert(key).inserted { viewer?.input(kind: .key, down: true, key: key) }
        }
    }
    private func syncModifiers(_ flags: NSEvent.ModifierFlags) {
        let modifiers: [(NSEvent.ModifierFlags, UInt16, UInt16)] = [(.control, 224, 228), (.shift, 225, 229), (.option, 226, 230), (.command, 227, 231)]
        for (flag, left, right) in modifiers where !flags.contains(flag) {
            for key in [left, right] where pressed.remove(key) != nil { viewer?.input(kind: .key, down: false, key: key) }
        }
        for (flag, left, right) in modifiers where flags.contains(flag) && !pressed.contains(left) && !pressed.contains(right) {
            pressed.insert(left); viewer?.input(kind: .key, down: true, key: left)
        }
    }
    public override func keyUp(with event: NSEvent) {
        guard inputEligible, window?.firstResponder === self else { return }
        syncModifiers(event.modifierFlags)
        if let key = RemoteKey.macToHID[event.keyCode], pressed.remove(key) != nil { viewer?.input(kind: .key, down: false, key: key) }
    }
    public override func flagsChanged(with event: NSEvent) {
        guard inputEligible, window?.firstResponder === self else { return }
        syncModifiers(event.modifierFlags)
    }
    public override func resignFirstResponder() -> Bool {
        releaseHeldInput()
        return super.resignFirstResponder()
    }
    // SwiftUI dismantles this view while invalidating its graph. Session state
    // belongs to RemoteDashboard.onDisappear; publishing here can crash it.
    func detach() {
        if let eventMonitor { NSEvent.removeMonitor(eventMonitor); self.eventMonitor = nil }
        frameSubscription?.cancel(); frameSubscription = nil; displayedFrame = nil
        viewer = nil; stopPointerCapture(); pressedButtons.removeAll(); pressed.removeAll()
        focusObservers.forEach(NotificationCenter.default.removeObserver); focusObservers.removeAll()
        track?.remove(video); track = nil; video.isHidden = true; snapshot.image = nil; snapshot.isHidden = true
    }
    public func insertText(_ string: Any, replacementRange: NSRange) {
        let text = (string as? NSAttributedString)?.string ?? (string as? String ?? "")
        if inputEligible, window?.firstResponder === self, !pointerCaptured, !text.isEmpty, text.utf8.count <= 4096 { viewer?.input(kind: .text, text: text) }; unmarkText()
    }
    public func setMarkedText(_ string: Any, selectedRange: NSRange, replacementRange: NSRange) {
        marked = (string as? NSAttributedString) ?? NSAttributedString(string: string as? String ?? "")
    }
    public func unmarkText() { marked = NSAttributedString(string: "") }
    public func selectedRange() -> NSRange { NSRange(location: 0, length: 0) }
    public func markedRange() -> NSRange { marked.length == 0 ? NSRange(location: NSNotFound, length: 0) : NSRange(location: 0, length: marked.length) }
    public func hasMarkedText() -> Bool { marked.length > 0 }
    public func attributedSubstring(forProposedRange range: NSRange, actualRange: NSRangePointer?) -> NSAttributedString? { nil }
    public func validAttributesForMarkedText() -> [NSAttributedString.Key] { [] }
    public func firstRect(forCharacterRange range: NSRange, actualRange: NSRangePointer?) -> NSRect {
        window?.convertToScreen(convert(CGRect(x: bounds.midX, y: bounds.midY, width: 1, height: 20), to: nil)) ?? .zero
    }
    public func characterIndex(for point: NSPoint) -> Int { 0 }
}
#else
import UIKit

public struct RemoteCanvas: UIViewRepresentable {
    @ObservedObject var viewer: RemoteViewer
    public init(viewer: RemoteViewer) { self.viewer = viewer }
    public func makeUIView(context: Context) -> TouchRemoteCanvas { TouchRemoteCanvas(viewer: viewer) }
    public func updateUIView(_ view: TouchRemoteCanvas, context: Context) { view.update(viewer) }
    public static func dismantleUIView(_ view: TouchRemoteCanvas, coordinator: ()) { view.detach() }
}

public final class TouchRemoteCanvas: UIView, UIScrollViewDelegate {
    let viewport = UIScrollView()
    private let content = UIView()
    private let video = RTCMTLVideoView()
    private let snapshot = UIImageView()
    private var viewportSize = CGSize.zero
    private var remoteDrag: UIPanGestureRecognizer!
    private var remoteScroll: UIPanGestureRecognizer!
    private weak var viewer: RemoteViewer?
    private var frameSubscription: AnyCancellable?
    private var displayedFrame: CGImage?
    private var track: RTCVideoTrack?
    private var surface = CGSize(width: 16, height: 9)
    private var dragOrigin: CGPoint?
    public override var canBecomeFirstResponder: Bool { viewer?.controlling == true }
    init(viewer: RemoteViewer) {
        self.viewer = viewer; super.init(frame: .zero)
        backgroundColor = .black
        viewport.delegate = self; viewport.minimumZoomScale = 1; viewport.maximumZoomScale = 6
        viewport.bouncesZoom = true; viewport.contentInsetAdjustmentBehavior = .never
        viewport.showsHorizontalScrollIndicator = false; viewport.showsVerticalScrollIndicator = false
        addSubview(viewport); viewport.addSubview(content)
        video.isUserInteractionEnabled = false; video.videoContentMode = .scaleAspectFit; content.addSubview(video)
        snapshot.contentMode = .scaleAspectFit; snapshot.isUserInteractionEnabled = false; content.addSubview(snapshot)
        let tap = UITapGestureRecognizer(target: self, action: #selector(tap(_:))); addGestureRecognizer(tap)
        remoteDrag = UIPanGestureRecognizer(target: self, action: #selector(drag(_:))); remoteDrag.maximumNumberOfTouches = 1; addGestureRecognizer(remoteDrag)
        remoteScroll = UIPanGestureRecognizer(target: self, action: #selector(scroll(_:))); remoteScroll.minimumNumberOfTouches = 2; addGestureRecognizer(remoteScroll)
        let secondary = UILongPressGestureRecognizer(target: self, action: #selector(secondary(_:))); addGestureRecognizer(secondary)
        tap.require(toFail: secondary)
        if let pinch = viewport.pinchGestureRecognizer { tap.require(toFail: pinch); secondary.require(toFail: pinch) }
        update(viewer)
    }
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
    func update(_ viewer: RemoteViewer) {
        let rebind = frameSubscription == nil || self.viewer !== viewer
        self.viewer = viewer
        if rebind {
            frameSubscription = viewer.$frame.sink { [weak self] image in self?.displayFrame(image) }
        }
        if let hand = viewer.hand { surface = CGSize(width: hand.width, height: hand.height) }
        if track !== viewer.track { track?.remove(video); track = viewer.track; track?.add(video) }
        video.isHidden = !viewer.connected || track == nil
        displayFrame(viewer.frame)
        if !viewer.controlling { dragOrigin = nil; resignFirstResponder() }
        updateGestures(); setNeedsLayout()
    }
    private func displayFrame(_ image: CGImage?) {
        if displayedFrame !== image {
            displayedFrame = image
            snapshot.image = image.map { UIImage(cgImage: $0) }
        }
        snapshot.isHidden = viewer?.connected != true || image == nil
    }
    public override func layoutSubviews() {
        super.layoutSubviews()
        viewport.frame = bounds
        if viewportSize != bounds.size {
            let scale = viewport.zoomScale
            viewport.setZoomScale(1, animated: false)
            viewportSize = bounds.size; content.frame = CGRect(origin: .zero, size: bounds.size)
            viewport.contentSize = bounds.size
            viewport.setZoomScale(scale, animated: false)
        }
        video.frame = fitted(surface, in: content.bounds); snapshot.frame = video.frame
    }
    public func viewForZooming(in scrollView: UIScrollView) -> UIView? { content }
    public func scrollViewWillBeginZooming(_ scrollView: UIScrollView, with view: UIView?) {
        remoteDrag.isEnabled = false; remoteScroll.isEnabled = false
        if dragOrigin != nil { viewer?.input(kind: .releaseAll); dragOrigin = nil }
    }
    public func scrollViewDidZoom(_ scrollView: UIScrollView) {
        accessibilityValue = "Zoom " + String(Int(scrollView.zoomScale * 100)) + "%"
        if !scrollView.isZooming { updateGestures() }
    }
    public func scrollViewDidEndZooming(_ scrollView: UIScrollView, with view: UIView?, atScale scale: CGFloat) { updateGestures() }
    private func updateGestures() {
        let controlling = viewer?.controlling == true
        remoteDrag.isEnabled = controlling && !viewport.isZooming
        remoteScroll.isEnabled = controlling && viewport.zoomScale <= 1.01 && !viewport.isZooming
        // Pinch always zooms locally. When magnified, two fingers pan the
        // viewport; one finger continues to control the remote pointer.
        viewport.panGestureRecognizer.minimumNumberOfTouches = controlling ? 2 : 1
        viewport.panGestureRecognizer.isEnabled = !controlling || viewport.zoomScale > 1.01
    }
    func normalizedPoint(_ location: CGPoint, clamp: Bool = false) -> CGPoint? {
        let point = content.convert(location, from: self), rect = video.frame
        guard rect.width > 0, rect.height > 0, clamp || rect.contains(point) else { return nil }
        return CGPoint(x: min(1, max(0, (point.x - rect.minX) / rect.width)), y: min(1, max(0, (point.y - rect.minY) / rect.height)))
    }
    private func click(_ location: CGPoint, button: Int) {
        guard let point = normalizedPoint(location) else { return }; becomeFirstResponder()
        for down in [true, false] { viewer?.input(kind: .button, x: point.x, y: point.y, button: button, down: down) }
    }
    @objc private func tap(_ gesture: UITapGestureRecognizer) { click(gesture.location(in: self), button: 0) }
    @objc private func secondary(_ gesture: UILongPressGestureRecognizer) {
        if gesture.state == .began { click(gesture.location(in: self), button: 1) }
    }
    @objc private func drag(_ gesture: UIPanGestureRecognizer) {
        let location = gesture.location(in: self)
        switch gesture.state {
        case .began:
            let translation = gesture.translation(in: self)
            guard let origin = normalizedPoint(CGPoint(x: location.x - translation.x, y: location.y - translation.y)) else { return }
            becomeFirstResponder(); dragOrigin = origin
            viewer?.input(kind: .button, x: origin.x, y: origin.y, button: 0, down: true)
        case .changed:
            if dragOrigin != nil, let point = normalizedPoint(location, clamp: true) { viewer?.input(kind: .move, x: point.x, y: point.y) }
        case .ended:
            if dragOrigin != nil, let point = normalizedPoint(location, clamp: true) { viewer?.input(kind: .button, x: point.x, y: point.y, button: 0, down: false) }
            dragOrigin = nil
        case .cancelled, .failed:
            if dragOrigin != nil { viewer?.input(kind: .releaseAll) }
            dragOrigin = nil
        default: break
        }
    }
    @objc private func scroll(_ gesture: UIPanGestureRecognizer) {
        guard gesture.state == .changed, let point = normalizedPoint(gesture.location(in: self)) else { return }
        let delta = gesture.translation(in: self); gesture.setTranslation(.zero, in: self)
        viewer?.input(kind: .scroll, x: point.x, y: point.y, deltaX: min(4096, max(-4096, delta.x)), deltaY: min(4096, max(-4096, delta.y)))
    }
    public override func pressesBegan(_ presses: Set<UIPress>, with event: UIPressesEvent?) { keys(presses, down: true) }
    public override func pressesEnded(_ presses: Set<UIPress>, with event: UIPressesEvent?) { keys(presses, down: false) }
    public override func pressesCancelled(_ presses: Set<UIPress>, with event: UIPressesEvent?) { keys(presses, down: false) }
    private func keys(_ presses: Set<UIPress>, down: Bool) {
        for press in presses { if let key = press.key, let code = UInt16(exactly: key.keyCode.rawValue), RemoteKey.supported(code) { viewer?.input(kind: .key, down: down, key: code) } }
    }
    // Keep teardown render-only; the owning dashboard closes the session.
    func detach() { frameSubscription?.cancel(); frameSubscription = nil; displayedFrame = nil; viewer = nil; track?.remove(video); track = nil; video.isHidden = true; snapshot.image = nil; snapshot.isHidden = true }
    public override func resignFirstResponder() -> Bool { viewer?.input(kind: .releaseAll); return super.resignFirstResponder() }
}
#endif
