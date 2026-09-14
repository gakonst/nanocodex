import SwiftUI
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

public struct RemoteCanvas: NSViewRepresentable {
    @ObservedObject var viewer: RemoteViewer
    public init(viewer: RemoteViewer) { self.viewer = viewer }
    public func makeNSView(context: Context) -> MacRemoteViewport { MacRemoteViewport(viewer: viewer) }
    public func updateNSView(_ view: MacRemoteViewport, context: Context) { view.canvas.update(viewer) }
    public static func dismantleNSView(_ view: MacRemoteViewport, coordinator: ()) { view.canvas.detach() }
}

public final class MacRemoteViewport: NSScrollView {
    let canvas: MacRemoteCanvas
    init(viewer: RemoteViewer) {
        canvas = MacRemoteCanvas(viewer: viewer)
        super.init(frame: .zero)
        drawsBackground = true; backgroundColor = .black
        allowsMagnification = true; minMagnification = 1; maxMagnification = 6
        hasHorizontalScroller = true; hasVerticalScroller = true; autohidesScrollers = true
        documentView = canvas
    }
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
    public override func layout() {
        super.layout()
        // Document coordinates stay stable while AppKit magnifies the clip view.
        let size = contentSize
        if canvas.frame.size != size { canvas.setFrameSize(size) }
    }
}

public final class MacRemoteCanvas: NSView, NSTextInputClient {
    private let video = RTCMTLNSVideoView()
    private let snapshot = NSImageView()
    private weak var viewer: RemoteViewer?
    private var track: RTCVideoTrack?
    private var surface = CGSize(width: 16, height: 9)
    private var pressed = Set<UInt16>()
    private var marked = NSAttributedString(string: "")
    private var dragging = false
    private var tracking: NSTrackingArea?
    public override var isFlipped: Bool { true }
    public override var acceptsFirstResponder: Bool { viewer?.controlling == true }
    init(viewer: RemoteViewer) {
        self.viewer = viewer
        super.init(frame: .zero)
        wantsLayer = true; layer?.backgroundColor = NSColor.black.cgColor
        snapshot.imageScaling = .scaleProportionallyUpOrDown
        addSubview(video); addSubview(snapshot); update(viewer)
    }
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
    func update(_ viewer: RemoteViewer) {
        self.viewer = viewer
        if let hand = viewer.hand { surface = CGSize(width: hand.width, height: hand.height) }
        if track !== viewer.track { track?.remove(video); track = viewer.track; track?.add(video) }
        video.isHidden = !viewer.connected || track == nil
        snapshot.image = viewer.frame.map { NSImage(cgImage: $0, size: .zero) }
        snapshot.isHidden = !viewer.connected || viewer.frame == nil
        if !viewer.controlling { pressed.removeAll(); dragging = false; unmarkText() }
        needsLayout = true
    }
    public override func layout() { super.layout(); video.frame = fitted(surface, in: bounds); snapshot.frame = video.frame }
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
        guard viewer?.controlling == true, let point = point(event, clamp: !down && dragging) else { return }
        if down { window?.makeFirstResponder(self) }; dragging = down
        viewer?.input(kind: .button, x: point.x, y: point.y, button: button, down: down)
    }
    public override func mouseDown(with event: NSEvent) { button(event, down: true, button: 0) }
    public override func mouseUp(with event: NSEvent) { button(event, down: false, button: 0) }
    public override func rightMouseDown(with event: NSEvent) { button(event, down: true, button: 1) }
    public override func rightMouseUp(with event: NSEvent) { button(event, down: false, button: 1) }
    public override func otherMouseDown(with event: NSEvent) { if event.buttonNumber == 2 { button(event, down: true, button: 2) } }
    public override func otherMouseUp(with event: NSEvent) { if event.buttonNumber == 2 { button(event, down: false, button: 2) } }
    public override func mouseMoved(with event: NSEvent) {
        guard let point = point(event, clamp: dragging) else { return }
        viewer?.input(kind: .move, x: point.x, y: point.y)
    }
    public override func mouseDragged(with event: NSEvent) { mouseMoved(with: event) }
    public override func rightMouseDragged(with event: NSEvent) { mouseMoved(with: event) }
    public override func otherMouseDragged(with event: NSEvent) { mouseMoved(with: event) }
    public override func scrollWheel(with event: NSEvent) {
        guard viewer?.controlling == true else { super.scrollWheel(with: event); return }
        guard let point = point(event) else { return }
        let scale: Double = event.hasPreciseScrollingDeltas ? 1 : 20
        viewer?.input(kind: .scroll, x: point.x, y: point.y,
            deltaX: min(4096, max(-4096, event.scrollingDeltaX * scale)), deltaY: min(4096, max(-4096, event.scrollingDeltaY * scale)))
    }
    public override func performKeyEquivalent(with event: NSEvent) -> Bool {
        guard window?.firstResponder === self, viewer?.controlling == true else { return false }
        if event.keyCode == 53, event.modifierFlags.contains([.command, .shift]) { viewer?.releaseControl(); return true }
        keyDown(with: event); return true
    }
    public override func keyDown(with event: NSEvent) {
        guard viewer?.controlling == true else { return }
        if viewer?.hand?.kind != .vm, event.modifierFlags.intersection([.command, .control]).isEmpty,
           let characters = event.characters, characters.unicodeScalars.allSatisfy({ $0.value >= 32 && $0.value < 0xF700 }) {
            interpretKeyEvents([event])
        } else if let key = RemoteKey.macToHID[event.keyCode] {
            pressed.insert(key); viewer?.input(kind: .key, down: true, key: key)
        }
    }
    public override func keyUp(with event: NSEvent) {
        if let key = RemoteKey.macToHID[event.keyCode], pressed.remove(key) != nil { viewer?.input(kind: .key, down: false, key: key) }
    }
    public override func flagsChanged(with event: NSEvent) {
        guard let key = RemoteKey.macToHID[event.keyCode] else { return }
        let flag: NSEvent.ModifierFlags = [224:.control, 225:.shift, 226:.option, 227:.command, 228:.control, 229:.shift, 230:.option, 231:.command][key] ?? []
        guard !flag.isEmpty else { return }
        let down = event.modifierFlags.contains(flag) && !pressed.contains(key)
        if down { pressed.insert(key) } else { pressed.remove(key) }
        viewer?.input(kind: .key, down: down, key: key)
    }
    public override func resignFirstResponder() -> Bool {
        viewer?.input(kind: .releaseAll); pressed.removeAll(); dragging = false; unmarkText()
        return super.resignFirstResponder()
    }
    // SwiftUI dismantles this view while invalidating its graph. Session state
    // belongs to RemoteDashboard.onDisappear; publishing here can crash it.
    func detach() { viewer = nil; track?.remove(video); track = nil; video.isHidden = true; snapshot.image = nil; snapshot.isHidden = true }
    public func insertText(_ string: Any, replacementRange: NSRange) {
        let text = (string as? NSAttributedString)?.string ?? (string as? String ?? "")
        if !text.isEmpty, text.utf8.count <= 4096 { viewer?.input(kind: .text, text: text) }; unmarkText()
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
        self.viewer = viewer
        if let hand = viewer.hand { surface = CGSize(width: hand.width, height: hand.height) }
        if track !== viewer.track { track?.remove(video); track = viewer.track; track?.add(video) }
        video.isHidden = !viewer.connected || track == nil
        snapshot.image = viewer.frame.map { UIImage(cgImage: $0) }
        snapshot.isHidden = !viewer.connected || viewer.frame == nil
        if !viewer.controlling { dragOrigin = nil; resignFirstResponder() }
        updateGestures(); setNeedsLayout()
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
    func detach() { viewer = nil; track?.remove(video); track = nil; video.isHidden = true; snapshot.image = nil; snapshot.isHidden = true }
    public override func resignFirstResponder() -> Bool { viewer?.input(kind: .releaseAll); return super.resignFirstResponder() }
}
#endif
