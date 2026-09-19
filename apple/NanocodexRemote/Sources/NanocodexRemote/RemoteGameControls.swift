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

#if os(iOS)
import SwiftUI
import UIKit

/// A keyboard/mouse touch layout. The host game's key bindings must match the labels.
@MainActor public struct RemoteGameControls: View {
    @ObservedObject private var viewer: RemoteViewer
    private let onClose: () -> Void
    @Environment(\.scenePhase) private var scenePhase
    @State private var input = RemoteGameInputState()
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
