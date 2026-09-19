import Foundation

/// Touch owners share keys without releasing a key another finger still holds.
struct RemoteGameInputState {
    enum Event: Equatable {
        case key(UInt16, Bool)
        case camera(Bool)
        case releaseAll
    }
    private(set) var owners: [String: Set<UInt16>] = [:]
    private(set) var cameraHeld = false
    var keys: Set<UInt16> { owners.values.reduce(into: []) { $0.formUnion($1) } }

    mutating func setKeys(_ newKeys: Set<UInt16>, owner: String) -> [Event] {
        let before = keys
        if newKeys.isEmpty { owners.removeValue(forKey: owner) } else { owners[owner] = newKeys }
        let after = keys
        return before.subtracting(after).sorted().map { .key($0, false) }
            + after.subtracting(before).sorted().map { .key($0, true) }
    }

    mutating func camera(_ down: Bool) -> [Event] {
        guard cameraHeld != down else { return [] }
        cameraHeld = down
        return [.camera(down)]
    }

    mutating func reset() -> [Event] {
        owners.removeAll(); cameraHeld = false
        return [.releaseAll]
    }

    static func movement(x: Double, y: Double) -> Set<UInt16> {
        guard x.isFinite, y.isFinite else { return [] }
        var result = Set<UInt16>()
        if x < -18 { result.insert(4) } // A
        if x > 18 { result.insert(7) } // D
        if y < -18 { result.insert(26) } // W
        if y > 18 { result.insert(22) } // S
        return result
    }
}

/// Owners are independent so releasing one surface cannot release another finger.
struct RemoteNativeGameInputState {
    private(set) var buttonsByOwner: [String: Set<String>] = [:]
    private(set) var sticks: [String: CGPoint] = [:]
    private(set) var triggersByOwner: [String: Set<String>] = [:]
    var triggers: Set<String> { Set(triggersByOwner.values.flatMap { $0 }) }
    var buttons: [String] { Set(buttonsByOwner.values.flatMap { $0 }).sorted() }
    var snapshot: RemoteGamepadState {
        let left = sticks["left"] ?? .zero
        let right = sticks["right"] ?? .zero
        return RemoteGamepadState(leftX: left.x, leftY: left.y, rightX: right.x, rightY: right.y,
            leftTrigger: triggers.contains("leftTrigger") ? 1 : 0,
            rightTrigger: triggers.contains("rightTrigger") ? 1 : 0, buttons: buttons)
    }
    var isNeutral: Bool { snapshot == RemoteGamepadState() }
    static let heartbeatNanoseconds: UInt64 = (1_000_000_000 + 29) / 30

    mutating func button(_ name: String, owner: String, down: Bool) {
        if down { buttonsByOwner[owner, default: []].insert(name) }
        else {
            buttonsByOwner[owner]?.remove(name)
            if buttonsByOwner[owner]?.isEmpty == true { buttonsByOwner.removeValue(forKey: owner) }
        }
    }
    mutating func stick(_ name: String, x: Double, y: Double) {
        guard x.isFinite, y.isFinite else { sticks.removeValue(forKey: name); return }
        let magnitude = hypot(x, y)
        // A radial dead zone prevents drift without clipping diagonal movement.
        guard magnitude > 0.12 else { sticks.removeValue(forKey: name); return }
        let radius = min(1, (magnitude - 0.12) / 0.88)
        sticks[name] = CGPoint(x: x / magnitude * radius, y: y / magnitude * radius)
    }
    mutating func trigger(_ name: String, owner: String? = nil, down: Bool) {
        let owner = owner ?? name
        if down { triggersByOwner[owner, default: []].insert(name) }
        else {
            triggersByOwner[owner]?.remove(name)
            if triggersByOwner[owner]?.isEmpty == true { triggersByOwner.removeValue(forKey: owner) }
        }
    }
    mutating func reset() { buttonsByOwner.removeAll(); sticks.removeAll(); triggersByOwner.removeAll() }
}

/// All coordinates are inside the safe-area content below the compact header.
struct RemoteNativeGameLayout {
    let width: CGFloat
    let height: CGFloat
    var stickSize: CGFloat { 112 }
    var buttonSize: CGFloat { 44 }
    var controlY: CGFloat { height - 86 }
    var leftStick: CGPoint { CGPoint(x: 64, y: controlY) }
    var rightStick: CGPoint { CGPoint(x: width - 64, y: controlY) }
    var dpad: CGPoint { CGPoint(x: width / 2 - 72, y: controlY) }
    var face: CGPoint { CGPoint(x: width / 2 + 72, y: controlY) }
    var fits: Bool { width >= 536 && height >= 218 }
}

#if os(iOS)
import SwiftUI
import UIKit

/// Transport-only state: a heartbeat never publishes a SwiftUI update.
@MainActor private final class RemoteNativeGameTransportPump: ObservableObject {
    private var heartbeat: Task<Void, Never>?
    private var latest = RemoteGamepadState()
    private var lastSent: TimeInterval = 0
    private weak var viewer: RemoteViewer?

    deinit { heartbeat?.cancel() }

    func update(_ snapshot: RemoteGamepadState, viewer: RemoteViewer, immediate: Bool) {
        self.viewer = viewer
        latest = snapshot
        if snapshot == RemoteGamepadState() {
            stop(viewer: viewer)
            return
        }
        if immediate || heartbeat == nil {
            viewer.gamepad(snapshot)
            lastSent = ProcessInfo.processInfo.systemUptime
        }
        guard heartbeat == nil else { return }
        // One bounded ticker refreshes held input before the 500ms host watchdog.
        heartbeat = Task { @MainActor [weak self] in
            while !Task.isCancelled {
                do { try await Task.sleep(nanoseconds: RemoteNativeGameInputState.heartbeatNanoseconds) }
                catch { return }
                guard !Task.isCancelled, self?.tick() == true else { return }
            }
        }
    }

    func stop(viewer: RemoteViewer) {
        heartbeat?.cancel(); heartbeat = nil
        latest = RemoteGamepadState()
        lastSent = 0
        self.viewer = nil
        viewer.gamepad(latest)
    }

    private func tick() -> Bool {
        guard let viewer, viewer.connected, viewer.controlling, viewer.supportsGamepad,
              latest != RemoteGamepadState() else {
            heartbeat = nil
            latest = RemoteGamepadState()
            return false
        }
        let now = ProcessInfo.processInfo.systemUptime
        guard now - lastSent >= 1.0 / 30 else { return true }
        viewer.gamepad(latest)
        lastSent = now
        return true
    }
}

/// Native controller input when supported, with the existing keyboard/mouse fallback.
@MainActor public struct RemoteGameControls: View {
    @ObservedObject private var viewer: RemoteViewer
    private let onClose: () -> Void
    @Environment(\.scenePhase) private var scenePhase
    @State private var input = RemoteGameInputState()
    @State private var nativeInput = RemoteNativeGameInputState()
    @StateObject private var nativePump = RemoteNativeGameTransportPump()
    @State private var paused = false
    @State private var epoch = 0
    @State private var stick = CGSize.zero
    @State private var cameraPoint: CGPoint?
    @State private var lastMotion: TimeInterval = 0

    public init(viewer: RemoteViewer, onClose: @escaping () -> Void) {
        self.viewer = viewer; self.onClose = onClose
    }

    private var enabled: Bool { viewer.connected && viewer.controlling && !paused && scenePhase == .active }

    private var cameraEnabled: Bool { enabled && viewer.supportsRelativePointer }

    public var body: some View {
        Group {
            if viewer.supportsGamepad { nativeBody } else { keyboardBody }
        }
        .onChange(of: viewer.supportsGamepad) { _, _ in stop() }
        .onChange(of: viewer.controlling) { _, value in if !value { stop() } }
        .onChange(of: viewer.connected) { _, value in if !value { stop() } }
        .onChange(of: scenePhase) { _, value in if value != .active { stop() } }
        .onDisappear { stop() }
    }

    private var keyboardBody: some View {
        GeometryReader { geometry in
            let landscape = geometry.size.width > geometry.size.height
            let padSize: CGFloat = landscape ? min(138, max(72, geometry.size.height - 240)) : 100
            VStack(spacing: 8) {
                HStack(spacing: 10) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("WoW · Keyboard + mouse").font(.caption.bold())
                        Text(landscape ? "Match your in-game key bindings" : "Rotate to landscape for more room")
                            .font(.caption2).foregroundStyle(.white.opacity(0.75))
                    }
                    Spacer(minLength: 0)
                    Button(paused ? "Resume" : "Stop", systemImage: paused ? "play.fill" : "stop.fill") {
                        if paused { paused = false; epoch += 1 } else { stop(); paused = true }
                    }
                    .tint(paused ? .mint : .red)
                    .accessibilityIdentifier("remote-game-stop")
                    Button { stop(); onClose() } label: { Image(systemName: "xmark").frame(width: 24, height: 24) }
                        .accessibilityLabel("Close game controls")
                        .accessibilityIdentifier("remote-game-close")
                }
                .buttonStyle(.borderedProminent)
                .padding(10).background(.black.opacity(0.65), in: RoundedRectangle(cornerRadius: 16))

                if !viewer.controlling || !viewer.connected || paused {
                    HStack {
                        Text(paused ? "Input paused" : viewer.connected ? "Take control to play" : "Screen disconnected")
                        if viewer.connected && !viewer.controlling {
                            Button("Take control") { viewer.takeControl() }.buttonStyle(.borderedProminent)
                        }
                    }.font(.caption.bold()).padding(8).background(.black.opacity(0.7), in: Capsule())
                }
                Spacer(minLength: 0)
                HStack(alignment: .bottom, spacing: 10) {
                    VStack(spacing: 8) {
                        HStack(spacing: 6) {
                            key("Shift", code: 225, id: "shift")
                            key("Ctrl", code: 224, id: "control")
                        }
                        joystick(size: padSize)
                    }.frame(width: padSize + 28)
                    Spacer(minLength: 0)
                    VStack(spacing: 8) {
                        HStack(spacing: 6) {
                            key("Esc", code: 41, id: "escape")
                            key("Tab · Target", code: 43, id: "tab")
                        }
                        camera(size: padSize)
                        key("Space · Jump", code: 44, id: "space")
                    }.frame(width: padSize + 28)
                }
                HStack(spacing: 8) {
                    ForEach(1...6, id: \.self) { number in
                        key(String(number), code: UInt16(29 + number), id: String(number))
                    }
                }
            }
            .padding(landscape ? 12 : 8)
            .foregroundStyle(.white)
            .onChange(of: geometry.size) { _, _ in stop() }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("remote-game-controls")
        .onChange(of: viewer.controlling) { _, value in if !value { stop() } }
        .onChange(of: viewer.supportsRelativePointer) { _, value in if !value { stop() } }
        .onChange(of: viewer.connected) { _, value in if !value { stop() } }
        .onChange(of: scenePhase) { _, value in if value != .active { stop() } }
        .onDisappear { stop() }
    }

    private var nativeBody: some View {
        GeometryReader { geometry in
            VStack(spacing: 6) {
                HStack(spacing: 8) {
                    Text("Native gamepad").font(.caption.bold())
                        .accessibilityIdentifier("remote-native-gamepad")
                    if paused { Text("Paused").font(.caption2) }
                    Spacer(minLength: 0)
                    if viewer.connected && !viewer.controlling {
                        Button("Take control") { viewer.takeControl() }
                    }
                    if !viewer.connected { Text("Disconnected").font(.caption2) }
                    Button(paused ? "Resume" : "Stop") {
                        if paused { paused = false; epoch += 1 } else { stop(); paused = true }
                    }
                    .tint(paused ? .mint : .red)
                    .accessibilityIdentifier("remote-game-stop")
                    Button { stop(); onClose() } label: {
                        Image(systemName: "xmark").frame(width: 24, height: 24)
                    }
                    .accessibilityLabel("Close game controls")
                    .accessibilityIdentifier("remote-game-close")
                }
                .buttonStyle(.bordered).frame(height: 44)
                .padding(.horizontal, 8)
                .background(.black.opacity(0.4), in: RoundedRectangle(cornerRadius: 14))
                GeometryReader { content in
                    let layout = RemoteNativeGameLayout(width: content.size.width, height: content.size.height)
                    if layout.fits {
                        ZStack(alignment: .topLeading) {
                            nativeStick("left", size: layout.stickSize).position(layout.leftStick)
                            nativeStick("right", size: layout.stickSize).position(layout.rightStick)
                            nativeDiamond(center: layout.dpad, labels: ["↑", "↓", "←", "→"],
                                          names: ["dpadUp", "dpadDown", "dpadLeft", "dpadRight"])
                            nativeDiamond(center: layout.face, labels: ["Y", "A", "X", "B"], names: ["y", "a", "x", "b"])
                            nativeButton("LT", name: "leftTrigger", trigger: true).position(x: 34, y: 24)
                            nativeButton("LB", name: "leftShoulder").position(x: 90, y: 24)
                            nativeButton("RT", name: "rightTrigger", trigger: true).position(x: layout.width - 34, y: 24)
                            nativeButton("RB", name: "rightShoulder").position(x: layout.width - 90, y: 24)
                            nativeButton("Back", name: "back").position(x: layout.width / 2 - 30, y: 24)
                            nativeButton("Start", name: "start").position(x: layout.width / 2 + 30, y: 24)
                            nativeButton("L3", name: "leftStick").position(x: 146, y: layout.height - 22)
                            nativeButton("R3", name: "rightStick").position(x: layout.width - 146, y: layout.height - 22)
                        }
                    } else {
                        Text("Rotate to landscape for gamepad controls")
                            .font(.callout.bold()).frame(maxWidth: .infinity, maxHeight: .infinity)
                            .background(.black.opacity(0.35))
                    }
                }
            }
            .padding(8).foregroundStyle(.white)
            .onChange(of: geometry.size) { _, _ in stop() }
        }
    }

    private func nativeDiamond(center: CGPoint, labels: [String], names: [String]) -> some View {
        let offsets = [CGPoint(x: 0, y: -46), CGPoint(x: 0, y: 46), CGPoint(x: -46, y: 0), CGPoint(x: 46, y: 0)]
        return ForEach(0..<4, id: \.self) { index in
            nativeButton(labels[index], name: names[index])
                .position(x: center.x + offsets[index].x, y: center.y + offsets[index].y)
        }
    }

    private func nativeButton(_ label: String, name: String, trigger: Bool = false) -> some View {
        let held = trigger ? nativeInput.triggers.contains(name) : nativeInput.buttons.contains(name)
        return Text(label).font(.system(size: 13, weight: .bold, design: .rounded))
            .foregroundStyle((["a": Color.green, "b": .red, "x": .cyan, "y": .yellow][name] ?? .white))
            .frame(width: 44, height: 44)
            .background(held ? Color.mint.opacity(0.65) : Color.black.opacity(0.3), in: Circle())
            .background(.ultraThinMaterial, in: Circle())
            .overlay(Circle().stroke(.white.opacity(0.35)))
            .overlay {
                RemoteGameTouchSurface(enabled: enabled, epoch: epoch) { phase, _ in
                    guard enabled, phase != .moved else { return }
                    if trigger { nativeInput.trigger(name, down: phase == .began) }
                    else { nativeInput.button(name, owner: name, down: phase == .began) }
                    sendNative(immediate: true)
                }
            }
            .opacity(enabled ? 1 : 0.45)
            .accessibilityElement(children: .ignore).accessibilityLabel(label)
            .accessibilityAddTraits(.isButton)
            .accessibilityIdentifier("remote-gamepad-\(name)")
            .accessibilityAction {
                guard enabled else { return }
                if trigger { nativeInput.trigger(name, owner: "accessibility", down: true) }
                else { nativeInput.button(name, owner: "accessibility", down: true) }
                sendNative(immediate: true)
                if trigger { nativeInput.trigger(name, owner: "accessibility", down: false) }
                else { nativeInput.button(name, owner: "accessibility", down: false) }
                sendNative(immediate: true)
            }
    }

    private func nativeStick(_ name: String, size: CGFloat) -> some View {
        let point = nativeInput.sticks[name] ?? .zero
        let travel = (size - 44) / 2
        return ZStack {
            Circle().fill(.black.opacity(0.22)).background(.ultraThinMaterial, in: Circle())
                .overlay(Circle().stroke(.white.opacity(0.35)))
            Circle().fill(.white.opacity(0.3)).frame(width: 44, height: 44)
                .offset(x: point.x * travel, y: point.y * travel)
        }
        .frame(width: size, height: size)
        .overlay {
            RemoteGameTouchSurface(enabled: enabled, epoch: epoch) { phase, location in
                guard enabled else { return }
                let wasHeld = nativeInput.sticks[name] != nil
                if phase == .ended {
                    nativeInput.stick(name, x: 0, y: 0)
                } else {
                    nativeInput.stick(name, x: (location.x - size / 2) / travel,
                                      y: (location.y - size / 2) / travel)
                }
                let released = wasHeld && nativeInput.sticks[name] == nil
                sendNative(immediate: phase != .moved || released)
            }
        }
        .opacity(enabled ? 1 : 0.45)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(name == "left" ? "Left analog joystick" : "Right analog joystick")
        .accessibilityIdentifier("remote-gamepad-\(name)-analog")
    }

    private func sendNative(immediate: Bool) {
        guard enabled && viewer.supportsGamepad else { return }
        nativePump.update(nativeInput.snapshot, viewer: viewer, immediate: immediate)
    }

    private func key(_ title: String, code: UInt16, id: String) -> some View {
        Text(title).font(.system(size: 13, weight: .bold, design: .rounded))
            .frame(maxWidth: .infinity).frame(minWidth: 42, minHeight: 44)
            .background(input.keys.contains(code) ? Color.mint.opacity(0.75) : Color.black.opacity(0.65),
                        in: RoundedRectangle(cornerRadius: 12))
            .overlay(RoundedRectangle(cornerRadius: 12).stroke(.white.opacity(0.3)))
            .overlay {
                RemoteGameTouchSurface(enabled: enabled, epoch: epoch) { phase, _ in
                    if phase == .began { emit(input.setKeys([code], owner: id)) }
                    if phase == .ended { emit(input.setKeys([], owner: id)) }
                }
            }
            .opacity(enabled ? 1 : 0.5)
            .accessibilityElement(children: .ignore).accessibilityLabel(title)
            .accessibilityAddTraits(.isButton)
            .accessibilityIdentifier("remote-game-key-\(id)")
            .accessibilityAction {
                guard enabled else { return }
                emit(input.setKeys([code], owner: "accessibility"))
                emit(input.setKeys([], owner: "accessibility"))
            }
    }

    private func joystick(size: CGFloat) -> some View {
        ZStack {
            Circle().fill(.black.opacity(0.5)).overlay(Circle().stroke(.white.opacity(0.3)))
            VStack { Text("W"); Spacer(); Text("S") }.padding(9)
            HStack { Text("A"); Spacer(); Text("D") }.padding(12)
            Circle().fill(.white.opacity(0.25)).frame(width: 44, height: 44).offset(stick)
        }
        .font(.caption.bold()).frame(width: size, height: size)
        .overlay {
            RemoteGameTouchSurface(enabled: enabled, epoch: epoch) { phase, point in
                if phase == .ended {
                    stick = .zero; emit(input.setKeys([], owner: "stick")); return
                }
                let x = point.x - size / 2, y = point.y - size / 2
                let scale = min(1, (size / 2 - 22) / max(1, hypot(x, y)))
                stick = CGSize(width: x * scale, height: y * scale)
                emit(input.setKeys(RemoteGameInputState.movement(x: x, y: y), owner: "stick"))
            }
        }
        .opacity(enabled ? 1 : 0.5)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("W A S D movement joystick")
        .accessibilityHint("Hold and drag from the center to move")
        .accessibilityIdentifier("remote-game-joystick")
    }

    private func camera(size: CGFloat) -> some View {
        VStack(spacing: 6) {
            Image(systemName: "viewfinder").font(.title2)
            Text(viewer.supportsRelativePointer ? "CAMERA" : "Camera needs desktop update")
                .font(.caption.bold()).multilineTextAlignment(.center)
            Text(viewer.supportsRelativePointer ? "Drag · Right mouse" : "Update the desktop host to aim")
                .font(.system(size: 10)).multilineTextAlignment(.center)
        }
        .frame(width: size + 28, height: size * 0.72)
        .background(input.cameraHeld ? Color.mint.opacity(0.35) : Color.black.opacity(0.5),
                    in: RoundedRectangle(cornerRadius: 24))
        .overlay(RoundedRectangle(cornerRadius: 24).stroke(.white.opacity(0.3)))
        .overlay {
            RemoteGameTouchSurface(enabled: cameraEnabled, epoch: epoch) { phase, point in
                guard cameraEnabled else { return }
                switch phase {
                case .began:
                    cameraPoint = point; lastMotion = 0; emit(input.camera(true))
                case .moved:
                    let now = ProcessInfo.processInfo.systemUptime
                    guard now - lastMotion >= 1.0 / 60, let previous = cameraPoint else { return }
                    cameraPoint = point; lastMotion = now
                    let dx = max(-128, min(128, point.x - previous.x))
                    let dy = max(-128, min(128, point.y - previous.y))
                    if dx != 0 || dy != 0 { viewer.input(kind: .relativeMove, deltaX: dx, deltaY: dy) }
                case .ended:
                    cameraPoint = nil; emit(input.camera(false))
                }
            }
        }
        .opacity(cameraEnabled ? 1 : 0.65)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(viewer.supportsRelativePointer
            ? "Camera: hold and drag to move with right mouse button"
            : "Camera needs desktop update. Keyboard controls remain available.")
        .accessibilityIdentifier("remote-game-camera")
    }

    private func emit(_ events: [RemoteGameInputState.Event]) {
        for event in events {
            switch event {
            case let .key(code, down): viewer.input(kind: .key, down: down, key: code)
            case let .camera(down): viewer.input(kind: .button, button: 1, down: down)
            case .releaseAll: viewer.input(kind: .releaseAll)
            }
        }
    }

    private func stop() {
        nativePump.stop(viewer: viewer)
        nativeInput.reset()
        emit(input.reset()); epoch += 1; stick = .zero; cameraPoint = nil; lastMotion = 0
    }
}

/// UIKit delivers cancellation and independent touches reliably, including fingers
/// dragged outside a control. A reset invalidates old touches before input resumes.
private struct RemoteGameTouchSurface: UIViewRepresentable {
    enum Phase { case began, moved, ended }
    var enabled: Bool
    var epoch: Int
    var action: (Phase, CGPoint) -> Void

    func makeUIView(context: Context) -> TouchView { TouchView() }
    func updateUIView(_ view: TouchView, context: Context) {
        if view.epoch != epoch || !enabled { view.touch = nil }
        view.epoch = epoch; view.action = action; view.isUserInteractionEnabled = enabled
    }

    static func dismantleUIView(_ view: TouchView, coordinator: ()) {
        // The parent owns releaseAll on disappearance. Never mutate SwiftUI
        // state from UIKit teardown while the view graph is being dismantled.
        view.touch = nil
        view.action = { _, _ in }
        view.isUserInteractionEnabled = false
    }

    final class TouchView: UIView {
        var touch: UITouch?
        var epoch = -1
        var action: (Phase, CGPoint) -> Void = { _, _ in }
        override init(frame: CGRect) {
            super.init(frame: frame)
            isMultipleTouchEnabled = true; backgroundColor = .clear
        }
        required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
        override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
            guard touch == nil, let first = touches.first else { return }
            touch = first; action(.began, first.location(in: self))
        }
        override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
            guard let touch, touches.contains(touch) else { return }
            action(.moved, touch.location(in: self))
        }
        override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) { finish(touches) }
        override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) { finish(touches) }
        private func finish(_ touches: Set<UITouch>) {
            guard let touch, touches.contains(touch) else { return }
            self.touch = nil; action(.ended, touch.location(in: self))
        }
    }
}
#endif
